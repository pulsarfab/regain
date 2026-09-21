#![cfg(feature = "caa")]
use anyhow::{Result, bail};
use regain_zwo::caa::{Caa, Transport, controller::Controller};
use serde_json::json;
use std::{cell::RefCell, rc::Rc};

struct State {
    position: u32,
    limit: u16,
    query: u8,
    moving: bool,
    instant: bool,
    reverse: bool,
    fail_move: bool,
    fail_read: bool,
    distance: f64,
    moves: usize,
    references: usize,
    stops: usize,
}
#[derive(Clone)]
struct Sim(Rc<RefCell<State>>);
impl Transport for Sim {
    fn set_output(&mut self, r: &[u8]) -> Result<()> {
        let mut s = self.0.borrow_mut();
        if r[3] == 2 {
            s.query = r[4];
        }
        if r[3] == 9 {
            s.reverse = r[4] != 0;
        }
        if r[3] == 3 {
            if r[4] == 2 {
                s.stops += 1;
                s.moving = false;
            } else if r[4] == 1 {
                let p = u32::from_be_bytes(r[6..10].try_into().unwrap());
                s.moves += 1;
                if s.instant {
                    s.distance += (f64::from(p) - f64::from(s.position)) / 10000.;
                    s.position = p;
                } else {
                    s.moving = true;
                }
                if s.fail_move {
                    bail!("uncertain accepted move");
                }
            } else if r[10] == 1 {
                s.position = u32::from_be_bytes(r[6..10].try_into().unwrap());
                s.references += 1;
            } else if r[10] == 2 {
                s.limit = u16::from_be_bytes([r[14], r[15]]);
            }
        }
        Ok(())
    }
    fn get_input(&mut self) -> Result<Vec<u8>> {
        let mut s = self.0.borrow_mut();
        if s.fail_read {
            s.fail_read = false;
            bail!("injected failed read");
        }
        let mut r = vec![0; 16];
        r[..4].copy_from_slice(&[1, 126, 90, s.query]);
        if s.query == 8 {
            r[4] = 1;
            r[5] = u8::from(s.reverse);
        }
        if s.query == 3 {
            r[4] = u8::from(s.moving);
            r[6..10].copy_from_slice(&s.position.to_be_bytes());
            r[13..15].copy_from_slice(&s.limit.to_be_bytes());
        }
        Ok(r)
    }
}
fn setup() -> (Controller<Sim>, Rc<RefCell<State>>) {
    let s = Rc::new(RefCell::new(State {
        position: 1_520_000,
        limit: 360,
        query: 0,
        moving: false,
        instant: true,
        reverse: false,
        fail_move: false,
        fail_read: false,
        distance: 0.,
        moves: 0,
        references: 0,
        stops: 0,
    }));
    (
        Controller::new(Caa::connect(Sim(s.clone())).unwrap()).unwrap(),
        s,
    )
}
#[test]
fn segmented_travel_preserves_sky_coordinates_in_both_directions() {
    for degrees in [450., -450.] {
        let (mut c, s) = setup();
        c.request(&json!({"command":"rotate-unwrapped","degrees":degrees}))
            .unwrap();
        for _ in 0..5 {
            c.tick();
        }
        let result = c.request(&json!({"command":"status"})).unwrap();
        assert_eq!(result["moving"], false);
        assert_eq!(
            result["logical_degrees"],
            json!((152.0_f64 + degrees).rem_euclid(360.))
        );
        assert_eq!(s.borrow().distance, degrees);
        assert_eq!(s.borrow().references, 5);
        assert_eq!(s.borrow().moves, 5);
        assert_eq!(s.borrow().limit, 360);
    }
}
#[test]
fn halt_does_not_start_the_next_segment() {
    let (mut c, s) = setup();
    c.request(&json!({"command":"rotate-unwrapped","degrees":450}))
        .unwrap();
    c.request(&json!({"command":"stop"})).unwrap();
    c.tick();
    assert_eq!(s.borrow().moves, 1);
    assert_eq!(s.borrow().stops, 1);
}
#[test]
fn uncertain_move_is_stopped_without_retry() {
    let (mut c, s) = setup();
    s.borrow_mut().fail_move = true;
    assert!(
        c.request(&json!({"command":"move-mechanical","degrees":154}))
            .is_err()
    );
    c.tick();
    assert_eq!(s.borrow().moves, 1);
    assert_eq!(s.borrow().stops, 1);
    assert!(
        c.request(&json!({"command":"move-mechanical","degrees":154}))
            .is_err()
    );
    assert!(c.request(&json!({"command":"status"})).unwrap()["motion_error"].is_string());
}
#[test]
fn read_failure_cancels_remaining_segments() {
    let (mut c, s) = setup();
    c.request(&json!({"command":"rotate-unwrapped","degrees":450}))
        .unwrap();
    s.borrow_mut().fail_read = true;
    c.tick();
    c.tick();
    assert_eq!(s.borrow().moves, 1);
    assert_eq!(s.borrow().stops, 1);
}
#[test]
fn opposite_motion_stops() {
    let (mut c, s) = setup();
    s.borrow_mut().instant = false;
    c.request(&json!({"command":"move-mechanical","degrees":154}))
        .unwrap();
    s.borrow_mut().position = 1_510_000;
    c.tick();
    assert_eq!(s.borrow().stops, 1);
}
#[test]
fn reference_preserves_logical_position_but_sync_never_writes_reference() {
    let (mut c, s) = setup();
    c.request(&json!({"command":"sync","degrees":40})).unwrap();
    assert_eq!(s.borrow().references, 0);
    c.request(&json!({"command":"reset-origin"})).unwrap();
    let status = c.request(&json!({"command":"status"})).unwrap();
    assert_eq!(status["logical_degrees"], 40.);
    assert_eq!(status["mechanical_degrees"], 0.);
    assert_eq!(s.borrow().moves, 0);
}
#[test]
fn extended_limit_requires_explicit_change() {
    let (mut c, s) = setup();
    assert!(
        c.request(&json!({"command":"move-mechanical","degrees":361}))
            .is_err()
    );
    assert_eq!(s.borrow().moves, 0);
    c.request(&json!({"command":"limit","degrees":361}))
        .unwrap();
    c.request(&json!({"command":"move-mechanical","degrees":361}))
        .unwrap();
    assert_eq!(s.borrow().position, 3_610_000);
    assert!(
        c.request(&json!({"command":"limit","degrees":362}))
            .is_err()
    );
}
