use super::*;

#[derive(Default)]
struct Device {
    position: f64,
    moving: bool,
    reverse: bool,
    bad_ack: bool,
    wire: Vec<String>,
}
impl Transport for Device {
    fn exchange(&mut self, command: &str) -> Result<String> {
        self.wire.push(command.into());
        Ok(match command {
            "F#" => "F2R_123456_A".into(),
            "FV" => "FV:1.8".into(),
            "FA" => format!(
                "F2R:{:.2}:{}:4500:4:{}",
                self.position,
                u8::from(self.moving),
                u8::from(self.reverse)
            ),
            "FH" => {
                self.moving = false;
                "FH:1".into()
            }
            "FN:0" | "FN:1" => {
                self.reverse = command.ends_with('1');
                command.into()
            }
            _ => {
                let (op, value) = command.split_once(':').unwrap();
                let value = value.parse::<f64>().unwrap();
                assert!((0.0..=360.).contains(&value));
                match op {
                    "MD" => {
                        self.position = value;
                        self.moving = true;
                    }
                    "SD" => self.position = value,
                    _ => panic!("Unexpected command {command}"),
                }
                if self.bad_ack {
                    "wrong".into()
                } else {
                    command.into()
                }
            }
        })
    }
}
fn device() -> Rotator<Device> {
    Rotator::new(Device::default()).unwrap()
}
fn near(a: f64, b: f64) {
    assert!(distance(a, b) < 0.02, "{a} != {b}");
}

#[test]
fn persisted_offset_restore_never_moves_changes_direction_or_resets_origin() {
    let mut d = device();
    d.transport.position = 152.0;
    d.transport.reverse = true;
    d.transport.wire.clear();
    d.request(&json!({"command":"restore-reference", "offset":250.5}))
        .unwrap();
    let state = d.status().unwrap();
    near(state.logical_degrees, 42.5);
    near(state.target_degrees, 42.5);
    near(state.mechanical_degrees, 152.0);
    assert!(state.reverse);
    assert!(d.transport.wire.iter().all(|command| command == "FA"));
    for offset in [json!(-1), json!(360), json!("0"), Value::Null] {
        assert!(
            d.request(&json!({"command":"restore-reference", "offset":offset}))
                .is_err()
        );
    }
    near(d.status().unwrap().logical_degrees, 42.5);
}

#[test]
fn strict_status_rejects_wrong_model_partial_flags_and_nonfinite_angles() {
    for wire in [
        "FR:0:0:4500:4:0",
        "F2R:NaN:0:4500:4:0",
        "F2R:361:0:4500:4:0",
        "F2R:0:2:4500:4:0",
        "F2R:0:0:4500:3:0",
        "F2R:0:0:4500:4:0:extra",
    ] {
        assert!(parse_status(wire).is_err(), "{wire}");
    }
    assert_eq!(
        parse_status("F2R:360.00:1:4500:4:1")
            .unwrap()
            .logical_degrees,
        0.
    );
}
#[test]
fn reference_reverse_and_sync_preserve_independent_coordinates() {
    let mut d = device();
    d.request(&json!({"command":"sync","degrees":42})).unwrap();
    d.request(&json!({"command":"reference","degrees":120}))
        .unwrap();
    near(d.status().unwrap().logical_degrees, 42.);
    d.request(&json!({"command":"reverse","enabled":true}))
        .unwrap();
    near(d.status().unwrap().logical_degrees, 42.);
    d.request(&json!({"command":"move-to","degrees":43}))
        .unwrap();
    assert_eq!(d.transport.wire.last().unwrap(), "MD:121.00");
    d.transport.moving = false;
    near(d.status().unwrap().logical_degrees, 43.);
    d.request(&json!({"command":"reset-origin"})).unwrap();
    let s = d.status().unwrap();
    near(s.logical_degrees, 43.);
    near(s.mechanical_degrees, 0.);
}
#[test]
fn segment_completion_preserves_total_travel_and_final_target() {
    for travel in [450_f64, -450.] {
        let mut d = device();
        d.request(&json!({"command":"sync","degrees":20})).unwrap();
        d.request(&json!({"command":"rotate-unwrapped","degrees":travel}))
            .unwrap();
        let mut segments = 0;
        while d.has_pending_motion() {
            segments += 1;
            assert!(segments <= 5);
            near(
                d.status().unwrap().target_degrees,
                (20. + travel).rem_euclid(360.),
            );
            d.transport.moving = false;
            d.status().unwrap();
        }
        assert_eq!(segments, 5);
        near(
            d.status().unwrap().logical_degrees,
            (20. + travel).rem_euclid(360.),
        );
        assert_eq!(
            d.transport
                .wire
                .iter()
                .filter(|s| s.starts_with("MD:"))
                .count(),
            5
        );
    }
}
#[test]
fn halt_drops_remaining_segments_without_starting_another() {
    let mut d = device();
    d.request(&json!({"command":"rotate-unwrapped","degrees":450}))
        .unwrap();
    d.transport.moving = false; // Segment finished just as halt arrived.
    d.halt().unwrap();
    d.status().unwrap();
    assert!(!d.has_pending_motion());
    assert_eq!(
        d.transport
            .wire
            .iter()
            .filter(|s| s.starts_with("MD:"))
            .count(),
        1
    );
}
#[test]
fn uncertain_move_is_never_replayed_and_latches_session() {
    let mut d = device();
    d.transport.bad_ack = true;
    let request = json!({"command":"move-to","degrees":20});
    assert!(d.request(&request).is_err());
    assert_eq!(
        d.transport
            .wire
            .iter()
            .filter(|s| s.as_str() == "FH")
            .count(),
        1
    );
    assert!(!d.transport.moving);
    let count = d.transport.wire.len();
    assert!(d.request(&request).is_err());
    assert!(d.status().is_err());
    assert_eq!(d.transport.wire.len(), count);
    assert_eq!(
        d.transport
            .wire
            .iter()
            .filter(|s| s.starts_with("MD:"))
            .count(),
        1
    );
}
#[test]
fn deadline_or_early_stop_halts_once_and_latches_fault() {
    for timeout in [true, false] {
        let mut d = device();
        d.request(&json!({"command":"move-to","degrees":20}))
            .unwrap();
        if timeout {
            d.motion.as_mut().unwrap().deadline = Instant::now();
        } else {
            d.transport.moving = false;
            d.transport.position = 10.;
        }
        assert!(d.status().is_err());
        assert!(d.fault().is_some());
        assert!(!d.has_pending_motion());
        assert_eq!(
            d.transport
                .wire
                .iter()
                .filter(|s| s.as_str() == "FH")
                .count(),
            1
        );
    }
}
#[test]
fn invalid_request_does_not_touch_transport() {
    let mut d = device();
    let count = d.transport.wire.len();
    for request in [
        json!({"command":"move-to","degrees":360}),
        json!({"command":"move-relative","degrees":361}),
        json!({"command":"rotate-unwrapped","degrees":451}),
        json!({"command":"sync","degrees":-1}),
    ] {
        assert!(d.request(&request).is_err());
    }
    assert_eq!(count, d.transport.wire.len());
}
