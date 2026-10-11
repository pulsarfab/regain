//! ASI585/662/676 clock wakeup, shared by still and video stop.
//! These sensors use the same FPGA gates but a different standby register from
//! ASI2600/6200. Preserve thermal/mode bits and keep cleanup noncancellable.
//! Evidence: SDK 1.41 ASI662/676 USB 2 long-video traces and ASI662 WorkingFunc
//! (Linux SDK address 0x13b730); see docs/asi662mc.md and docs/asi676-video.md.
//! Gate names describe their observed acquisition role, not verified pin wiring.
use crate::asi::direct::transport::Camera;
use anyhow::{Result, ensure};
use std::time::Duration;

trait Io {
    fn read(&self, register: u16) -> Result<u8>;
    fn write(&self, register: u16, value: u8) -> Result<()>;
    fn sleep(&self, duration: Duration);
}
impl Io for Camera {
    fn read(&self, register: u16) -> Result<u8> {
        Ok(self.vendor(0xbc, register, 0, 1)?[0])
    }
    fn write(&self, register: u16, value: u8) -> Result<()> {
        self.vendor(0xbd, register, u16::from(value), 0)?;
        Ok(())
    }
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

pub(super) fn restore_if_gated(camera: &Camera) -> Result<()> {
    restore(camera)
}

fn restore(io: &impl Io) -> Result<()> {
    let thermal = io.read(0x19)?;
    let clock = io.read(0x0b)?;
    if thermal & 1 == 0 && clock & 0x11 == 0 {
        return Ok(());
    }
    // SDK video end/stop sequence: clear FPGA 19 bit 0 (low-power gate), allow
    // the traced 200 ms wake interval, clear 0b bit 4 (called XHS in the SDK
    // analysis), then clear 0b bit 0 (integration hold). BC19 also carries
    // thermal flags, so writing a literal zero would change unrelated controls.
    // Fresh BC0b reads preserve flags changed during the preceding write.
    io.write(0x19, thermal & !1)?;
    io.sleep(Duration::from_millis(200));
    let clock = io.read(0x0b)?;
    io.write(0x0b, clock & !0x10)?;
    let clock = io.read(0x0b)?;
    io.write(0x0b, clock & !1)?;
    let thermal = io.read(0x19)?;
    let clock = io.read(0x0b)?;
    ensure!(
        thermal & 1 == 0 && clock & 0x11 == 0,
        "Bayer camera clock cleanup failed: BC19={thermal:02x}, BC0b={clock:02x}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asi::direct::{asi585, asi662, asi676, completion};
    use std::cell::{Cell, RefCell};

    struct Registers {
        thermal: Cell<u8>,
        clock: Cell<u8>,
        writes: RefCell<Vec<(u16, u8)>>,
        sleeps: RefCell<Vec<Duration>>,
        ignore: Option<(u16, u8)>,
        fail_read: Cell<bool>,
    }
    impl Registers {
        fn new(thermal: u8, clock: u8) -> Self {
            Self {
                thermal: Cell::new(thermal),
                clock: Cell::new(clock),
                writes: RefCell::new(Vec::new()),
                sleeps: RefCell::new(Vec::new()),
                ignore: None,
                fail_read: Cell::new(false),
            }
        }
    }
    impl Io for Registers {
        fn read(&self, register: u16) -> Result<u8> {
            ensure!(!self.fail_read.get(), "USB read failed");
            Ok(match register {
                0x19 => self.thermal.get(),
                0x0b => self.clock.get(),
                _ => panic!("unexpected register {register:x}"),
            })
        }
        fn write(&self, register: u16, value: u8) -> Result<()> {
            self.writes.borrow_mut().push((register, value));
            if self.ignore == Some((register, value)) {
                return Ok(());
            }
            match register {
                0x19 => self.thermal.set(value),
                0x0b => self.clock.set(value),
                _ => panic!("unexpected register {register:x}"),
            }
            Ok(())
        }
        fn sleep(&self, duration: Duration) {
            self.sleeps.borrow_mut().push(duration);
        }
    }

    #[test]
    fn stopped_clocks_are_ready_for_next_capture_at_every_gated_stage() {
        // The same helper serves all profiles; no ASI2600/6200 sensor register
        // or thermal actuator command is introduced into their stop paths.
        for profile in [&asi585::PROFILE, &asi662::PROFILE, &asi676::PROFILE] {
            for thermal in [0, 0x40, 0x80, 0xc0] {
                for (low_power, hold) in [(0, 1), (1, 1), (1, 0x11), (0, 0x11)] {
                    let io = Registers::new(thermal | low_power, 0x20 | hold);
                    restore(&io).unwrap_or_else(|e| panic!("{}: {e}", profile.name));
                    assert_eq!(io.thermal.get(), thermal, "{}", profile.name);
                    assert_eq!(io.clock.get(), 0x20, "{}", profile.name);
                    assert_eq!(*io.sleeps.borrow(), [Duration::from_millis(200)]);
                    assert_eq!(io.writes.borrow().last(), Some(&(0x0b, 0x20)));
                    // Another integration in this same register state can be
                    // cancelled and restored without needing firmware reset.
                    io.clock.set(io.clock.get() | 1);
                    restore(&io).unwrap();
                    assert_eq!(io.clock.get(), 0x20);
                    assert_eq!(io.thermal.get(), thermal);
                }
            }
        }
    }

    #[test]
    fn clean_stop_does_not_write_or_delay_unrelated_flags() {
        let io = Registers::new(0xc0, 0x20);
        restore(&io).unwrap();
        assert!(io.writes.borrow().is_empty());
        assert!(io.sleeps.borrow().is_empty());
    }

    #[test]
    fn ignored_gate_writes_cannot_acknowledge_a_clean_abort() {
        for ignored in [(0x19, 0xc0), (0x0b, 0x21), (0x0b, 0x20)] {
            let mut io = Registers::new(0xc1, 0x31);
            io.ignore = Some(ignored);
            let stopped = Cell::new(false);
            let cleanup = completion::finish_stop("Bayer camera", restore(&io), || {
                stopped.set(true);
                Ok(())
            });
            let error =
                completion::finish(Err(completion::CaptureCancelled.into()), cleanup).unwrap_err();
            assert!(stopped.get(), "full stop must still be attempted");
            assert!(!error.is::<completion::CaptureCancelled>());
            assert!(error.to_string().contains("clock cleanup failed"));
        }
    }

    #[test]
    fn wake_read_error_still_attempts_stop_and_preserves_both_failures() {
        let io = Registers::new(1, 0x11);
        io.fail_read.set(true);
        let stopped = Cell::new(false);
        let error = completion::finish_stop("Bayer camera", restore(&io), || {
            stopped.set(true);
            anyhow::bail!("AA failed")
        })
        .unwrap_err();
        assert!(stopped.get());
        assert!(error.to_string().contains("USB read failed"));
        assert!(error.to_string().contains("AA failed"));
    }
}
