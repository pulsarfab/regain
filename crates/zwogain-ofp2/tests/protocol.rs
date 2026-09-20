use anyhow::{Result, bail};
use std::{
    collections::VecDeque,
    io::Cursor,
    sync::{Arc, Mutex},
};
use zwogain_ofp2::{CoverState, Panel, Transport, serial::read_frame};

struct Script {
    replies: VecDeque<&'static str>,
    writes: Arc<Mutex<Vec<String>>>,
}
impl Transport for Script {
    fn exchange(&mut self, command: &str) -> Result<String> {
        self.writes.lock().unwrap().push(command.into());
        match self.replies.pop_front().expect("Unexpected command") {
            "TIMEOUT" => bail!("Read timed out"),
            v => Ok(v.into()),
        }
    }
}
fn panel(extra: &[&'static str]) -> (Panel<Script>, Arc<Mutex<Vec<String>>>) {
    let writes = Arc::new(Mutex::new(vec![]));
    let replies = [
        "Board=DeepSkyDad.FP2, Version=1.0.14.2",
        "3",
        "1",
        "0",
        "0",
        "0",
    ]
    .into_iter()
    .chain(extra.iter().copied())
    .collect();
    (
        Panel::new(Script {
            replies,
            writes: writes.clone(),
        })
        .unwrap(),
        writes,
    )
}
#[test]
fn captured_frames_and_malformed_responses() {
    for (wire, expected) in [
        ("(OK)", "OK"),
        ("(4096)", "4096"),
        (
            "(Board=DeepSkyDad.FP2, Version=1.0.14.2)",
            "Board=DeepSkyDad.FP2, Version=1.0.14.2",
        ),
    ] {
        assert_eq!(read_frame(&mut Cursor::new(wire)).unwrap(), expected);
    }
    for wire in ["!100)", "(OK", "garbage)", "()", "((OK)", "(\n)", "(é)"] {
        assert!(read_frame(&mut Cursor::new(wire)).is_err(), "{wire}");
    }
    assert!(read_frame(&mut Cursor::new(format!("({})", "x".repeat(256)))).is_err());
}
#[test]
fn response_can_arrive_after_serial_read_poll_timeout() {
    struct Delayed {
        timeout: bool,
        bytes: Cursor<&'static [u8]>,
    }
    impl std::io::Read for Delayed {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            if self.timeout {
                self.timeout = false;
                return Err(std::io::ErrorKind::TimedOut.into());
            }
            std::io::Read::read(&mut self.bytes, bytes)
        }
    }
    assert_eq!(
        read_frame(&mut Delayed {
            timeout: true,
            bytes: Cursor::new(b"(OK)")
        })
        .unwrap(),
        "OK"
    );
}
#[test]
fn captured_light_sequence_bounds_and_zero_on() {
    let (mut p, writes) = panel(&["OK", "OK", "1", "0", "1", "128", "OK", "OK", "OK", "OK"]);
    assert!(p.light_on(4097).is_err());
    assert_eq!(writes.lock().unwrap().len(), 6);
    p.light_on(128).unwrap();
    let status = p.status().unwrap();
    assert!(status.light_on);
    assert_eq!(status.brightness, 128);
    p.light_on(0).unwrap();
    p.light_off().unwrap();
    assert_eq!(
        &writes.lock().unwrap()[6..],
        [
            "SLBR128", "SLON1", "GOPS", "GPOS", "GLON", "GLBR", "SLBR0", "SLON1", "SLBR0", "SLON0"
        ]
    );
}
#[test]
fn zero_brightness_preserves_logical_on_despite_physical_glon_zero() {
    let (mut p, _) = panel(&[
        "OK", "OK", "1", "0", "0", "0", "OK", "OK", "1", "0", "0", "0",
    ]);
    p.light_on(0).unwrap();
    let s = p.status().unwrap();
    assert!(s.calibrator_on);
    assert!(!s.light_on);
    p.light_off().unwrap();
    assert!(!p.status().unwrap().calibrator_on);
}
#[test]
fn partial_mutation_faults_without_replay_or_followup_write() {
    let (mut p, writes) = panel(&["TIMEOUT"]);
    assert!(p.light_on(128).is_err());
    assert!(p.light_on(128).is_err());
    assert!(p.status().is_err());
    assert_eq!(&writes.lock().unwrap()[6..], ["SLBR128"]);
    assert!(p.fault().is_some());
}
#[test]
fn movement_is_polled_and_halt_is_distinct_from_an_endpoint() {
    let (mut p, writes) = panel(&[
        "1", "0", "0", "0", "OK", "OK", "2", "42", "0", "0", "OK", "1", "232", "0", "0",
    ]);
    p.move_cover(false).unwrap();
    assert!(p.has_pending_motion());
    assert_eq!(p.status().unwrap().cover, CoverState::Moving);
    p.halt().unwrap();
    assert!(!p.has_pending_motion());
    assert_eq!(p.status().unwrap().cover, CoverState::Unknown);
    assert_eq!(&writes.lock().unwrap()[10..12], ["STRG270", "SMOV"]);
}
#[test]
fn premature_idle_does_not_claim_success() {
    let (mut p, _) = panel(&["1", "0", "0", "0", "OK", "OK", "1", "232"]);
    p.move_cover(false).unwrap();
    assert!(p.status().is_err());
    assert!(p.fault().is_some());
}
#[test]
fn wrong_board_or_product_is_never_actuated() {
    for replies in [
        vec!["Board=Other"],
        vec!["Board=DeepSkyDad.FP2, Version=1", "2"],
    ] {
        let writes = Arc::new(Mutex::new(vec![]));
        assert!(
            Panel::new(Script {
                replies: replies.into(),
                writes: writes.clone()
            })
            .is_err()
        );
        assert!(writes.lock().unwrap().iter().all(|c| c.starts_with('G')));
    }
}
