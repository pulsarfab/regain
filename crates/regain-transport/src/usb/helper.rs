//! Fixed USB helper protocol and authorization policy; no platform side effects.
use super::Target;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub(super) const PROTOCOL_VERSION: u32 = 1;
pub(super) const MAX_TARGETS: usize = 16;
pub(super) const OPERATION_SECONDS: u64 = 60;
// Sixteen maximum-length approved instance paths, including JSON escaping.
pub(super) const MAX_MESSAGE: usize = 64 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(
    tag = "command",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub(super) enum Request {
    Status {
        protocol_version: u32,
        target: Option<Target>,
    },
    Approve {
        protocol_version: u32,
        target: Target,
    },
    Revoke {
        protocol_version: u32,
        target: Target,
    },
    Recover {
        protocol_version: u32,
        target: Target,
        cycle: bool,
        deadline_unix_seconds: u64,
    },
    Stop {
        protocol_version: u32,
    },
}
impl Request {
    pub fn validate(&self) -> Result<()> {
        let version = match self {
            Self::Status {
                protocol_version, ..
            }
            | Self::Approve {
                protocol_version, ..
            }
            | Self::Revoke {
                protocol_version, ..
            }
            | Self::Recover {
                protocol_version, ..
            }
            | Self::Stop { protocol_version } => *protocol_version,
        };
        ensure!(
            version == PROTOCOL_VERSION,
            "Unsupported USB helper protocol version"
        );
        match self {
            Self::Status {
                target: Some(target),
                ..
            }
            | Self::Approve { target, .. }
            | Self::Revoke { target, .. }
            | Self::Recover { target, .. } => target.validate()?,
            _ => {}
        }
        Ok(())
    }
}

pub(super) struct Broker {
    targets: Vec<Target>,
}
impl Broker {
    pub fn new(target: Target) -> Result<Self> {
        let mut broker = Self { targets: vec![] };
        broker.approve(target, true)?;
        Ok(broker)
    }
    pub fn approved(&self, target: &Target) -> bool {
        self.targets.iter().any(|approved| {
            approved.vendor == target.vendor
                && approved.product == target.product
                && approved.serial.eq_ignore_ascii_case(&target.serial)
                && approved.location.eq_ignore_ascii_case(&target.location)
        })
    }
    pub fn approve(&mut self, target: Target, elevated: bool) -> Result<()> {
        ensure!(
            elevated,
            "Adding a USB camera requires explicit elevated preparation"
        );
        target.validate()?;
        ensure!(
            target.generation.is_some_and(|generation| generation != 0),
            "USB helper preparation requires a verified arrival generation"
        );
        if let Some(existing) = self.targets.iter_mut().find(|approved| {
            approved.vendor == target.vendor
                && approved.product == target.product
                && approved.serial.eq_ignore_ascii_case(&target.serial)
                && approved.location.eq_ignore_ascii_case(&target.location)
        }) {
            *existing = target;
        } else {
            ensure!(
                self.targets.len() < MAX_TARGETS,
                "USB helper supports at most 16 approved cameras; stop it and prepare the required cameras"
            );
            self.targets.push(target);
        }
        Ok(())
    }
    pub fn revoke(&mut self, target: &Target) -> Result<Value> {
        target.validate()?;
        let before = self.targets.len();
        // A camera may already have disconnected/re-enumerated. Revocation
        // removes the approved physical binding without probing live hardware
        // or requiring the original arrival generation to remain current.
        self.targets.retain(|approved| {
            approved.vendor != target.vendor
                || approved.product != target.product
                || !approved.serial.eq_ignore_ascii_case(&target.serial)
                || !approved.location.eq_ignore_ascii_case(&target.location)
        });
        Ok(
            json!({"protocolVersion":PROTOCOL_VERSION,"revoked":self.targets.len() < before,
            "targetsRemaining":self.targets.len(),"stopped":self.targets.is_empty()}),
        )
    }
    pub fn authorize(&self, target: &Target, deadline: u64, now: u64) -> Result<()> {
        target.validate()?;
        ensure!(
            self.approved(target),
            "Camera is not approved by the USB reset helper; explicitly prepare this camera before observing"
        );
        ensure!(
            target.generation.is_some_and(|generation| generation != 0),
            "USB reset requires a verified camera arrival generation"
        );
        ensure!(
            now < deadline && deadline <= now.saturating_add(OPERATION_SECONDS + 5),
            "USB reset authorization expired or deadline is invalid"
        );
        Ok(())
    }
    pub fn status(&self, target: Option<&Target>) -> Result<Value> {
        // The Windows process is scoped to one authenticated user/session and
        // ends on explicit Stop or Windows logoff/reboot. Each recovery still
        // needs a fresh bounded request and a verified live device generation.
        let mut value = json!({"protocolVersion":PROTOCOL_VERSION,"running":true,"elevated":true,
            "sessionScoped":true,"expiresAtUnixSeconds":null,"targets":self.targets});
        if let Some(target) = target {
            target.validate()?;
            value["approved"] = json!(self.approved(target));
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn target(index: u16) -> Target {
        Target {
            vendor: 0x03c3,
            product: 0x620b,
            serial: format!("{index:016x}"),
            location: format!("USB\\VID_03C3&PID_620B\\CAMERA{index}"),
            address: None,
            generation: Some(123),
        }
    }
    #[test]
    fn approvals_are_explicit_and_bounded_to_verified_targets() {
        let mut broker = Broker::new(target(1)).unwrap();
        assert!(broker.approve(target(2), false).is_err());
        assert!(!broker.approved(&target(2)));
        for index in 2..=16 {
            broker.approve(target(index), true).unwrap();
        }
        broker.approve(target(1), true).unwrap();
        let mut rebound = target(1);
        rebound.generation = Some(456);
        broker.approve(rebound, true).unwrap();
        assert_eq!(
            broker.status(Some(&target(1))).unwrap()["targets"][0]["generation"],
            456
        );
        assert!(broker.approve(target(17), true).is_err());
    }
    #[test]
    fn recovery_binds_serial_product_location_and_a_fresh_bounded_request() {
        let broker = Broker::new(target(1)).unwrap();
        let mut rebound = target(1);
        rebound.generation = Some(456);
        broker.authorize(&rebound, 161, 101).unwrap();
        // The Windows backend separately verifies this request generation
        // against the current OS arrival before issuing any native reset.
        rebound.serial = target(2).serial;
        assert!(broker.authorize(&rebound, 161, 101).is_err());
        rebound = target(1);
        rebound.location.push('x');
        assert!(broker.authorize(&rebound, 161, 101).is_err());
        rebound = target(1);
        rebound.product = 0x676d;
        assert!(broker.authorize(&rebound, 161, 101).is_err());
        rebound = target(1);
        rebound.generation = None;
        assert!(broker.authorize(&rebound, 161, 101).is_err());
        assert!(broker.authorize(&target(1), 101, 101).is_err());
        assert!(broker.authorize(&target(1), 167, 101).is_err());
        let status = broker.status(Some(&target(1))).unwrap();
        assert_eq!(status["approved"], true);
        assert!(status.get("peripheralDisconnectAcknowledged").is_none());
        assert_eq!(status["targets"].as_array().unwrap().len(), 1);
    }
    #[test]
    fn session_authorization_has_no_age_limit_but_each_request_expires() {
        let broker = Broker::new(target(1)).unwrap();
        let later = 100 + 30 * 24 * 60 * 60;
        broker.authorize(&target(1), later + 60, later).unwrap();
        assert!(broker.authorize(&target(1), 160, later).is_err());
        assert!(broker.authorize(&target(1), later + 66, later).is_err());
        let status = broker.status(Some(&target(1))).unwrap();
        assert_eq!(status["sessionScoped"], true);
        assert!(status.get("expiresAtUnixSeconds").unwrap().is_null());
        assert_eq!(status["approved"], true);
    }
    #[test]
    fn revocation_is_per_binding_idempotent_and_stops_after_last_target() {
        let mut broker = Broker::new(target(1)).unwrap();
        broker.approve(target(2), true).unwrap();
        let mut absent = target(1);
        absent.location.push('x');
        let response = broker.revoke(&absent).unwrap();
        assert_eq!(response["revoked"], false);
        assert_eq!(response["targetsRemaining"], 2);
        assert_eq!(response["stopped"], false);

        let mut rebound = target(1);
        rebound.location = rebound.location.to_ascii_lowercase();
        rebound.generation = None;
        let response = broker.revoke(&rebound).unwrap();
        assert_eq!(response["revoked"], true);
        assert_eq!(response["targetsRemaining"], 1);
        assert_eq!(response["stopped"], false);
        assert!(!broker.approved(&target(1)));
        assert!(broker.approved(&target(2)));
        assert!(broker.authorize(&target(1), 161, 101).is_err());
        broker.authorize(&target(2), 161, 101).unwrap();

        assert_eq!(broker.revoke(&rebound).unwrap()["revoked"], false);
        let response = broker.revoke(&target(2)).unwrap();
        assert_eq!(response["revoked"], true);
        assert_eq!(response["targetsRemaining"], 0);
        assert_eq!(response["stopped"], true);
    }
    #[test]
    fn protocol_rejects_unknown_fields_commands_and_versions() {
        assert!(
            serde_json::from_str::<Request>(
                r#"{"command":"execute","protocolVersion":1,"path":"cmd.exe"}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<Request>(
                r#"{"command":"stop","protocolVersion":1,"target":null}"#
            )
            .is_err()
        );
        let bad: Request =
            serde_json::from_str(r#"{"command":"status","protocolVersion":2,"target":null}"#)
                .unwrap();
        assert!(bad.validate().is_err());
        let request = Request::Status {
            protocol_version: PROTOCOL_VERSION,
            target: None,
        };
        let encoded = serde_json::to_vec(&request).unwrap();
        assert!(encoded.len() < MAX_MESSAGE);
        serde_json::from_slice::<Request>(&encoded)
            .unwrap()
            .validate()
            .unwrap();
    }
}
