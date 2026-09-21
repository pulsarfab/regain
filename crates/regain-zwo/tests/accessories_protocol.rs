#![cfg(feature = "accessories")]
use regain_zwo::accessories::{Accessory, Kind, Transport, decode_status, simulation::Sim};
use std::{cell::RefCell, rc::Rc};
struct Recording {
    sim: Sim,
    writes: Rc<RefCell<Vec<Vec<u8>>>>,
    fail_motion: bool,
}
#[test]
fn calibration_matches_trace_and_requires_busy_then_healthy_idle() {
    let writes = Rc::new(RefCell::new(Vec::new()));
    let mut device = Accessory::open(
        Recording {
            sim: Sim::new(Kind::Efw),
            writes: writes.clone(),
            fail_motion: false,
        },
        Kind::Efw,
    )
    .unwrap();
    let mut calibration = device.calibrate().unwrap();
    assert_eq!(
        writes.borrow().last().unwrap(),
        &[3, 126, 90, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
    );
    let idle = decode_status(
        Kind::Efw,
        &[1, 126, 90, 1, 1, 0, 1, 1, 1, 7, 0, 0, 0, 0, 7, 0],
    )
    .unwrap();
    assert!(
        !calibration
            .observe(&idle, std::time::Instant::now())
            .unwrap()
    );
    for count in 1..=7 {
        let status = decode_status(
            Kind::Efw,
            &[1, 126, 90, 1, 0, 0, 1, 1, 1, count, 0, 0, 0, 0, 3, 0],
        )
        .unwrap();
        assert!(
            !calibration
                .observe(&status, std::time::Instant::now())
                .unwrap()
        );
        assert_eq!(calibration.slots, 7);
    }
    assert!(
        calibration
            .observe(&idle, std::time::Instant::now())
            .unwrap()
    );
    // A repeated command while the first is running must not reach USB.
    let count = writes.borrow().iter().filter(|b| b[3] == 1).count();
    assert!(device.calibrate().is_err());
    assert_eq!(writes.borrow().iter().filter(|b| b[3] == 1).count(), count);
}
#[test]
fn calibration_faults_on_timeout_changed_slots_errors_and_wrong_final_position() {
    let idle = decode_status(
        Kind::Efw,
        &[1, 126, 90, 1, 1, 0, 1, 1, 1, 7, 0, 0, 0, 0, 7, 0],
    )
    .unwrap();
    for failure in 0..4 {
        let mut device = Accessory::open(Sim::new(Kind::Efw), Kind::Efw).unwrap();
        let mut calibration = device.calibrate().unwrap();
        let now = std::time::Instant::now();
        let mut status = idle.clone();
        status.moving = true;
        assert!(!calibration.observe(&status, now).unwrap());
        status = idle.clone();
        let mut observed = now;
        match failure {
            0 => observed += std::time::Duration::from_secs(91),
            1 => status.slots = 6,
            2 => status.error = 5,
            _ => status.position = 2,
        }
        assert!(calibration.observe(&status, observed).is_err());
    }
}
#[test]
fn uncertain_calibration_write_is_not_retried_and_eaf_is_rejected() {
    for kind in [Kind::Efw, Kind::Eaf] {
        let writes = Rc::new(RefCell::new(Vec::new()));
        let mut device = Accessory::open(
            Recording {
                sim: Sim::new(kind),
                writes: writes.clone(),
                fail_motion: true,
            },
            kind,
        )
        .unwrap();
        assert!(device.calibrate().is_err());
        assert_eq!(
            writes.borrow().iter().filter(|b| b[3] != 2).count(),
            usize::from(kind == Kind::Efw)
        );
    }
}
impl Transport for Recording {
    fn set_output(&mut self, b: &[u8]) -> anyhow::Result<()> {
        self.writes.borrow_mut().push(b.to_vec());
        if self.fail_motion && b[3] != 2 {
            anyhow::bail!("uncertain USB completion");
        }
        self.sim.set_output(b)
    }
    fn get_input(&mut self) -> anyhow::Result<Vec<u8>> {
        self.sim.get_input()
    }
}
#[test]
fn captured_readonly_reports() {
    let efw = [1, 126, 90, 1, 1, 0, 1, 1, 1, 7, 0, 0, 0, 0, 7, 0];
    let s = decode_status(Kind::Efw, &efw).unwrap();
    assert_eq!((s.position, s.slots, s.moving), (0, 7, false));
    let eaf = [1, 126, 90, 3, 0, 0, 0, 0, 155, 149, 0, 128, 82, 1, 234, 96];
    let s = decode_status(Kind::Eaf, &eaf).unwrap();
    assert_eq!(s.position, 39829);
    assert_eq!(s.max_step, 60000);
    assert_eq!(s.temperature_c, Some(28.5));
    assert!(s.beep);
    assert!(!s.reverse);
}
#[test]
fn reject_malformed_stale_and_disagreeing_reports() {
    assert!(decode_status(Kind::Eaf, &[0; 15]).is_err());
    assert!(
        decode_status(
            Kind::Efw,
            &[1, 126, 90, 3, 0, 0, 0, 0, 155, 149, 0, 128, 82, 1, 234, 96]
        )
        .is_err()
    );
    let mut b = [1, 126, 90, 1, 1, 0, 1, 2, 1, 7, 0, 0, 0, 0, 7, 0];
    assert!(decode_status(Kind::Efw, &b).is_err());
    b[4] = 2;
    assert_eq!(decode_status(Kind::Efw, &b).unwrap().position, -1);
    b[4] = 6;
    b[5] = 5;
    assert_eq!(decode_status(Kind::Efw, &b).unwrap().error, 5);
}
#[test]
fn eaf_writes_match_captured_sdk_packets_and_keep_settings() {
    let writes = Rc::new(RefCell::new(Vec::new()));
    let mut d = Accessory::open(
        Recording {
            sim: Sim::new(Kind::Eaf),
            writes: writes.clone(),
            fail_motion: false,
        },
        Kind::Eaf,
    )
    .unwrap();
    d.move_to(39879, false).unwrap();
    assert_eq!(
        writes.borrow().last().unwrap(),
        &[3, 126, 90, 3, 1, 0, 0, 0, 155, 199, 0, 0, 0, 1, 234, 96]
    );
    d.halt().unwrap();
    assert_eq!(
        writes.borrow().iter().rev().find(|b| b[3] != 2).unwrap()[4],
        0
    );
    d.settings(Some(false), Some(true), Some(1), Some(100000))
        .unwrap();
    let s = d.status().unwrap();
    assert_eq!(s.max_step, 100000);
    assert_eq!(s.backlash, 1);
    assert!(!s.beep);
    assert!(s.reverse);
    d.move_to(99999, false).unwrap();
    assert_eq!(d.status().unwrap().position, 99999);
}
#[test]
fn invalid_values_never_write_motion_and_uncertain_writes_are_not_retried() {
    let writes = Rc::new(RefCell::new(Vec::new()));
    let mut d = Accessory::open(
        Recording {
            sim: Sim::new(Kind::Eaf),
            writes: writes.clone(),
            fail_motion: true,
        },
        Kind::Eaf,
    )
    .unwrap();
    assert!(d.move_to(-1, false).is_err());
    assert!(d.move_to(60001, false).is_err());
    assert!(d.settings(None, None, None, Some(100)).is_err());
    assert!(writes.borrow().iter().all(|b| b[3] == 2));
    assert!(d.move_to(39879, false).is_err());
    assert_eq!(writes.borrow().iter().filter(|b| b[3] != 2).count(), 1);
}
#[test]
fn wheel_positions_and_direction_are_encoded_once() {
    let writes = Rc::new(RefCell::new(Vec::new()));
    let mut d = Accessory::open(
        Recording {
            sim: Sim::new(Kind::Efw),
            writes: writes.clone(),
            fail_motion: false,
        },
        Kind::Efw,
    )
    .unwrap();
    assert!(d.move_to(7, false).is_err());
    d.move_to(6, false).unwrap();
    assert_eq!(
        &writes.borrow().last().unwrap()[..6],
        &[3, 126, 90, 1, 2, 7]
    );
    d.move_to(0, true).unwrap();
    assert_eq!(
        &writes.borrow().last().unwrap()[..6],
        &[3, 126, 90, 1, 3, 1]
    );
    assert_eq!(d.status().unwrap().position, 0);
    assert!(d.halt().is_err());
}

struct DelayedStop {
    sim: Sim,
    remaining: usize,
    stop_sent: bool,
    stop_count: Rc<RefCell<usize>>,
}
impl Transport for DelayedStop {
    fn set_output(&mut self, b: &[u8]) -> anyhow::Result<()> {
        if b[3] == 3 && b[4] == 0 {
            self.stop_sent = true;
            *self.stop_count.borrow_mut() += 1;
        }
        self.sim.set_output(b)
    }
    fn get_input(&mut self) -> anyhow::Result<Vec<u8>> {
        let mut b = self.sim.get_input()?;
        if b[3] == 3 && self.stop_sent && self.remaining > 0 {
            b[4] = 1;
            self.remaining -= 1;
        }
        Ok(b)
    }
}
#[test]
fn halt_waits_for_device_idle_without_repeating_the_stop_write() {
    let count = Rc::new(RefCell::new(0));
    let mut d = Accessory::open(
        DelayedStop {
            sim: Sim::new(Kind::Eaf),
            remaining: 2,
            stop_sent: false,
            stop_count: count.clone(),
        },
        Kind::Eaf,
    )
    .unwrap();
    d.halt().unwrap();
    assert!(!d.status().unwrap().moving);
    assert_eq!(*count.borrow(), 1);
}
#[test]
fn halt_times_out_when_hardware_does_not_stop_without_replaying() {
    let count = Rc::new(RefCell::new(0));
    let mut d = Accessory::open(
        DelayedStop {
            sim: Sim::new(Kind::Eaf),
            remaining: usize::MAX,
            stop_sent: false,
            stop_count: count.clone(),
        },
        Kind::Eaf,
    )
    .unwrap();
    assert!(d.halt().unwrap_err().to_string().contains("one second"));
    assert_eq!(*count.borrow(), 1);
}
