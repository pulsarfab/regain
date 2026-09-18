use anyhow::{Result, bail};
use std::{collections::VecDeque, time::Duration};
use zwogain_caa::{Caa, Transport};

#[derive(Default)]
struct Fake {
    replies: VecDeque<Vec<u8>>,
    writes: Vec<Vec<u8>>,
    fail_write: bool,
    fail_reverse: bool,
}
impl Transport for &mut Fake {
    fn set_output(&mut self, report: &[u8]) -> Result<()> {
        self.writes.push(report.to_vec());
        if self.fail_reverse && report[3] == 9 {
            bail!("uncertain reverse write");
        }
        if self.fail_write && report[3] == 3 && report[4] == 1 {
            bail!("uncertain write");
        }
        Ok(())
    }
    fn get_input(&mut self) -> Result<Vec<u8>> {
        self.replies
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("read failed"))
    }
}
fn status(angle: u32, state: u8, error: u8) -> Vec<u8> {
    let mut r = vec![1, 126, 90, 3, state, 0, 0, 0, 0, 0, 0, 0, 0, 1, 104, error];
    r[6..10].copy_from_slice(&angle.to_be_bytes());
    r
}
fn settings(reverse: bool) -> Vec<u8> {
    vec![
        1,
        126,
        90,
        8,
        1,
        u8::from(reverse),
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ]
}
fn fake(replies: Vec<Vec<u8>>) -> Fake {
    let mut f = Fake::default();
    f.replies.push_back(settings(false));
    f.replies.extend(replies);
    f
}

#[test]
fn recorded_status_and_move_encoding() {
    let mut f = fake(vec![status(1_520_000, 0, 0), status(1_520_000, 0, 0)]);
    let mut c = Caa::connect(&mut f).unwrap();
    let s = c.status().unwrap();
    assert_eq!(s.mechanical_degrees, 152.0);
    assert_eq!(s.limit_degrees, 360);
    assert_eq!(s.temperature_c, None);
    c.move_mechanical(154.0).unwrap();
    // Captured SDK CAAMoveTo(154), excluding the SDK's stale reserved bytes.
    assert_eq!(
        f.writes.last().unwrap(),
        &[3, 126, 90, 3, 1, 0, 0, 23, 127, 160, 0, 0, 0, 0, 1, 104]
    );
}

#[test]
fn invalid_input_never_reaches_usb() {
    let mut f = fake(vec![]);
    let mut c = Caa::connect(&mut f).unwrap();
    for value in [-1.0, 361.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(c.move_mechanical(value).is_err());
        assert!(c.move_to(value).is_err());
        assert!(c.sync(value).is_err());
    }
    for value in [361.0, -361.0, f64::NAN] {
        assert!(c.move_relative(value).is_err());
    }
    assert!(c.set_limit(0).is_err());
    assert!(c.set_limit(361).is_err());
    assert_eq!(f.writes.len(), 1);
}

#[test]
fn malformed_short_stale_and_out_of_range_replies_fail() {
    for reply in [
        vec![0; 3],
        vec![0; 16],
        settings(false),
        status(3_600_001, 0, 0),
    ] {
        let mut f = fake(vec![reply]);
        assert!(Caa::connect(&mut f).unwrap().status().is_err());
    }
}

#[test]
fn corrupt_limits_and_unrepresentable_deadline_are_rejected() {
    for limit in [0_u16, 361, u16::MAX] {
        let mut r = status(1_520_000, 0, 0);
        r[13..15].copy_from_slice(&limit.to_be_bytes());
        let mut f = fake(vec![r]);
        assert!(
            Caa::connect(&mut f)
                .unwrap()
                .move_mechanical(154.0)
                .is_err()
        );
        assert!(f.writes.iter().all(|r| r[3] == 2));
    }
    let mut f = fake(vec![]);
    let mut c = Caa::connect(&mut f).unwrap();
    assert!(c.wait_for(152.0, Duration::MAX).is_err());
    assert_eq!(f.writes.len(), 1);
}

#[test]
fn moving_fault_or_limit_refuses_motion() {
    for reply in [status(1_520_000, 1, 0), status(1_520_000, 0, 2), {
        let mut r = status(1_520_000, 0, 0);
        r[13] = 0;
        r[14] = 153;
        r
    }] {
        let mut f = fake(vec![reply]);
        assert!(
            Caa::connect(&mut f)
                .unwrap()
                .move_mechanical(154.0)
                .is_err()
        );
        assert_eq!(f.writes.len(), 2); // settings query and status query only
    }
}

#[test]
fn stop_does_not_require_a_successful_status_read() {
    let mut f = fake(vec![]);
    let mut c = Caa::connect(&mut f).unwrap();
    assert!(c.status().is_err());
    c.stop().unwrap();
    assert_eq!(&f.writes.last().unwrap()[..5], &[3, 126, 90, 3, 2]);
}

#[test]
fn motion_is_not_retried_after_uncertain_write() {
    let mut f = fake(vec![status(1_520_000, 0, 0)]);
    f.fail_write = true;
    assert!(
        Caa::connect(&mut f)
            .unwrap()
            .move_mechanical(154.0)
            .is_err()
    );
    assert_eq!(
        f.writes.iter().filter(|r| r[3] == 3 && r[4] == 1).count(),
        1
    );
}

#[test]
fn failed_wait_stops_without_restarting_motion() {
    let mut f = fake(vec![status(1_520_000, 0, 0)]);
    let mut c = Caa::connect(&mut f).unwrap();
    c.move_mechanical(154.0).unwrap();
    assert!(c.wait_for(154.0, Duration::from_secs(1)).is_err());
    assert_eq!(
        f.writes.iter().filter(|r| r[3] == 3 && r[4] == 1).count(),
        1
    );
    assert_eq!(&f.writes.last().unwrap()[..5], &[3, 126, 90, 3, 2]);
}

#[test]
fn sync_zero_is_local_and_does_not_reset_mechanical_zero() {
    let mut f = fake(vec![status(1_520_000, 0, 0), status(1_520_000, 0, 0)]);
    let mut c = Caa::connect(&mut f).unwrap();
    c.sync(0.0).unwrap();
    let s = c.status().unwrap();
    assert_eq!(s.logical_degrees, 0.0);
    assert_eq!(s.mechanical_degrees, 152.0);
    assert!(f.writes.iter().all(|r| r[3] == 2));
}

#[test]
fn reverse_preserves_logical_position() {
    let mut f = fake(vec![
        status(1_520_000, 0, 0),
        settings(true),
        status(1_520_000, 0, 0),
        status(1_520_000, 0, 0),
        status(1_520_000, 0, 0),
    ]);
    let mut c = Caa::connect(&mut f).unwrap();
    c.set_reverse(true).unwrap();
    assert_eq!(c.status().unwrap().logical_degrees, 152.0);
    c.move_relative(1.0).unwrap();
    assert_eq!(
        &f.writes.last().unwrap()[6..10],
        &1_510_000_u32.to_be_bytes()
    );
}

#[test]
fn preserve_mechanical_360_endpoint() {
    let mut f = fake(vec![status(3_590_000, 0, 0), status(3_590_000, 0, 0)]);
    Caa::connect(&mut f).unwrap().move_relative(1.0).unwrap();
    assert_eq!(
        &f.writes.last().unwrap()[6..10],
        &3_600_000_u32.to_be_bytes()
    );
}

#[test]
fn uncertain_reverse_is_reconciled_before_logical_move() {
    let mut f = fake(vec![
        status(1_520_000, 0, 0),
        vec![0; 3],
        settings(true),
        status(1_520_000, 0, 0),
    ]);
    f.fail_reverse = true;
    let mut c = Caa::connect(&mut f).unwrap();
    assert!(c.set_reverse(true).is_err());
    c.move_to(153.0).unwrap();
    assert_eq!(
        &f.writes.last().unwrap()[6..10],
        &1_510_000_u32.to_be_bytes()
    );
}

#[test]
fn alias_validation_and_encoding() {
    let mut r = vec![1, 126, 90, 13, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    r[4..12].copy_from_slice(b"MyRotatr");
    let mut f = fake(vec![status(1_520_000, 0, 0), r]);
    let mut c = Caa::connect(&mut f).unwrap();
    for value in ["123456789", "\n", "\u{2603}"] {
        assert!(c.set_alias(value).is_err());
    }
    c.set_alias("MyRotatr").unwrap();
    assert_eq!(
        f.writes[2],
        [
            3, 126, 90, 13, 77, 121, 82, 111, 116, 97, 116, 114, 0, 0, 0, 0
        ]
    );
}

#[test]
fn temperature_conversion_and_missing_probe() {
    let mut replies = Vec::new();
    for adc in [0_u16, 615, 616, 931, 1020, 1021] {
        let mut r = status(0, 0, 0);
        r[11..13].copy_from_slice(&adc.to_be_bytes());
        replies.push(r);
    }
    let mut f = fake(replies);
    let mut c = Caa::connect(&mut f).unwrap();
    assert!(c.status().unwrap().temperature_c.is_none());
    assert!(c.status().unwrap().temperature_c.is_none());
    assert!(c.status().unwrap().temperature_c.is_some());
    let t = c.status().unwrap().temperature_c.unwrap();
    assert!((t - 25.0).abs() < 0.1);
    assert!(c.status().unwrap().temperature_c.is_none()); // outside the NTC table
    assert!(c.status().unwrap().temperature_c.is_none());
}
