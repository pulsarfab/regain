//! SDK 1.41 ASI6200 long-exposure handshake and clock restoration.
//! Cancellation is observed between complete register transactions, never
//! between a synchronization pulse's edges or during clock restoration.
use super::Camera;
use anyhow::{Result, ensure};
use std::time::{Duration, Instant};

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

/// Preserve cooler/auxiliary bits in BD19. SDK worker RVA 1ed575..1ed60b
/// reaches this sequence even when its integration cancellation flag is set.
/// Do not call service() here: it can throw cancellation before clocks resume.
fn restore_clocks(io: &impl Io) -> Result<()> {
    let flags = io.read(0x19)?;
    io.write(0xbd, 0x19, flags & !1)?;
    io.sleep(Duration::from_millis(100));
    io.write(0xb6, 0x19e, 1)?;
    io.sleep(Duration::from_millis(100));
    let flags = io.read(0xb)?;
    io.write(0xbd, 0xb, flags & !0x10)?;
    let flags = io.read(0xb)?;
    io.write(0xbd, 0xb, flags & !1)?;
    verify_clocks(io)
}

fn verify_clocks(io: &impl Io) -> Result<()> {
    let gate = io.read(0x19)?;
    let exposure = io.read(0xb)?;
    ensure!(
        gate & 1 == 0 && exposure & 0x11 == 0,
        "ASI6200 clocks did not resume: BC19={gate:#04x}, BC0b={exposure:#04x}"
    );
    Ok(())
}

/// Also repair gates left by an earlier worker before arming another frame.
/// BC23=1 only proves DDR was cleared; it does not prove these gates are open.
pub(super) fn restore_if_gated(io: &impl Io) -> Result<()> {
    let gate = io.read(0x19)?;
    let exposure = io.read(0xb)?;
    if gate & 1 != 0 || exposure & 0x11 != 0 {
        crate::asi::direct::diagnostics::log(
            "warning",
            "capture.stale_clocks",
            format_args!(
                "ASI6200 stop restoring stale clocks: BC19={gate:#04x}, BC0b={exposure:#04x}"
            ),
        );
        io.phase("restoring_clocks");
        restore_clocks(io)?;
    }
    Ok(())
}

pub(super) fn expose(io: &impl Io, microseconds: u32) -> Result<()> {
    io.phase("synchronizing");
    let mut stage = "synchronizing";
    let result = (|| {
        let mut synchronized = false;
        for _ in 0..5 {
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
        ensure!(synchronized, "ASI6200 long exposure synchronization failed");
        let flags = io.read(0xb)?;
        io.write(0xbd, 0xb, flags | 1)?;
        io.phase("integrating");
        stage = "integrating";
        let exposure_ms = microseconds / 1000;
        if exposure_ms > 1000 {
            let trigger = io.now();
            let mut iteration = 0;
            loop {
                match iteration {
                    6 => io.write(0xb6, 0x19e, 5)?,
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
                if elapsed >= u128::from(exposure_ms - 200) {
                    break;
                }
            }
        } else {
            io.service()?;
            io.sleep(Duration::from_millis(u64::from(exposure_ms - 200)));
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
                "ASI6200 {stage}, {microseconds} us: {error:#}; restoring clocks; BC00/0b/19/23={registers:02x?}"
            ),
            serde_json::json!({"phase":stage,"microseconds":microseconds,
                "BC00":registers[0],"BC0b":registers[1],
                "BC19":registers[2],"BC23":registers[3]}),
        );
    }
    // Includes synchronization/USB/environment errors, not just cancellation.
    // A partial register transaction must never be acknowledged as a clean stop.
    io.phase("restoring_clocks");
    let cleanup = restore_clocks(io);
    match (result, cleanup) {
        (Ok(()), cleanup) => cleanup,
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(cleanup)) => anyhow::bail!(
            "ASI6200 exposure failed: {error:#}; clock restoration failed: {cleanup:#}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asi::direct::completion::CaptureCancelled;
    use std::cell::{Cell, RefCell};

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
    }
    impl Default for Registers {
        fn default() -> Self {
            let mut fpga = [0; 0x24];
            fpga[0] = 0xc0;
            fpga[0xb] = 0x44; // Unrelated FPGA flags must survive.
            fpga[0x19] = 0xc0; // Cooler and heater remain enabled.
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
            Ok(self.fpga.borrow()[usize::from(register)])
        }
        fn write(&self, request: u8, register: u16, value: u8) -> Result<()> {
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
                0xb6 if register == 0x19e => self.standby.set(value),
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
        for millis in [0, 2, 24, 25, 625, 825, 1025, 1525] {
            let io = Registers::default();
            io.cancel_at.set(Some(Duration::from_millis(millis)));
            let error = expose(&io, 2_000_000).unwrap_err();
            assert!(error.is::<CaptureCancelled>(), "{millis}: {error:#}");
            io.assert_clocks_restored();
            assert!(io.elapsed.get() < Duration::from_secs(2));
            io.cancel_at.set(None);
            io.fpga.borrow_mut()[0x23] = 1;
            expose(&io, 2_000_000).unwrap();
            io.assert_clocks_restored();
        }
    }

    #[test]
    fn successful_sequence_matches_sdk_clock_restore_order() {
        let io = Registers::default();
        expose(&io, 2_000_000).unwrap();
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
        io.cancel_at.set(Some(Duration::from_millis(2000)));
        expose(&io, 2_000_000).unwrap();
        io.assert_clocks_restored();
        // The acquisition caller observes the pending cancellation at its
        // next ready/readout checkpoint, after restoration has finished.
        assert!(io.service().unwrap_err().is::<CaptureCancelled>());
    }

    #[test]
    fn environment_error_also_restores_clocks_without_masking_error() {
        let io = Registers {
            fail_service_at: Some(Duration::from_millis(825)),
            ..Registers::default()
        };
        assert_eq!(
            expose(&io, 2_000_000).unwrap_err().to_string(),
            "temperature read failed"
        );
        io.assert_clocks_restored();
    }

    #[test]
    fn usb_failure_after_gating_also_unwinds_for_reuse() {
        let io = Registers::default();
        io.fail_write.set(Some((0xbd, 0xb, 0x55)));
        assert_eq!(
            expose(&io, 2_000_000).unwrap_err().to_string(),
            "USB control transfer failed"
        );
        io.assert_clocks_restored();
        io.fpga.borrow_mut()[0x23] = 1;
        expose(&io, 2_000_000).unwrap();
    }

    #[test]
    fn one_second_mode_honors_early_cancellation_and_restores_clocks() {
        let io = Registers::default();
        io.cancel_at.set(Some(Duration::ZERO));
        assert!(expose(&io, 1_000_000).unwrap_err().is::<CaptureCancelled>());
        io.assert_clocks_restored();
        assert_eq!(io.elapsed.get(), Duration::from_millis(225));
    }

    #[test]
    fn stale_gates_are_repaired_without_changing_thermal_flags() {
        let io = Registers::default();
        io.fpga.borrow_mut()[0xb] |= 0x11;
        io.fpga.borrow_mut()[0x19] |= 1;
        io.standby.set(5);
        // Old cleanup could see BC23=1 here and report success.
        assert_eq!(io.read(0x23).unwrap(), 1);
        restore_if_gated(&io).unwrap();
        io.assert_clocks_restored();
        expose(&io, 2_000_000).unwrap();
    }

    #[test]
    fn unsuccessful_readback_is_not_acknowledged_as_clean_cancellation() {
        let io = Registers {
            ignore_gate_clear: true,
            ..Registers::default()
        };
        io.cancel_at.set(Some(Duration::from_millis(825)));
        let error = expose(&io, 2_000_000).unwrap_err();
        assert!(!error.is::<CaptureCancelled>());
        assert!(error.to_string().contains("clocks did not resume"));
        assert!(restore_if_gated(&io).is_err());
    }

    #[test]
    fn synchronization_failure_remains_bounded_and_restores_clocks() {
        let io = Registers {
            synchronize: false,
            ..Registers::default()
        };
        assert_eq!(
            expose(&io, 2_000_000).unwrap_err().to_string(),
            "ASI6200 long exposure synchronization failed"
        );
        assert_eq!(io.elapsed.get(), Duration::from_millis(325));
        io.assert_clocks_restored();
    }
}
