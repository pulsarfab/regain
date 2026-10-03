//! Validate the selected camera's active bulk transport independently of its
//! sensor profile. USB 2 fallback is negotiated by the bus, not by register writes.
use anyhow::{Context, Result, ensure};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    HighSpeed,
    SuperSpeed,
}

impl Link {
    pub fn label(self) -> &'static str {
        match self {
            Self::HighSpeed => "USB 2 high-speed (512-byte bulk packets)",
            Self::SuperSpeed => "USB 3 SuperSpeed (1024-byte bulk packets)",
        }
    }
}

/// Called before sensor/environment writes, and again by standalone capture.
/// bcdUSB alone is a specification revision, not proof of the active endpoint
/// layout. Reject malformed/ambiguous configurations rather than guessing.
pub fn validate(info: &Value, product: u32) -> Result<Link> {
    ensure!(
        info["vendorId"] == 0x03c3 && info["productId"] == product,
        "camera USB identity differs: expected VID 0x03c3 PID 0x{product:04x}, observed VID {} PID {}",
        info["vendorId"],
        info["productId"]
    );
    let revision = info["usbVersionBcd"]
        .as_u64()
        .context("missing USB specification revision")?;
    let endpoints = info["endpoints"]
        .as_array()
        .context("missing USB endpoints")?;
    let candidates: Vec<_> = endpoints
        .iter()
        .filter(|e| e["address"] == 0x81 && e["alternate"] == 0)
        .collect();
    ensure!(
        candidates.len() == 1,
        "camera USB layout differs: expected exactly one endpoint 0x81 in alternate setting 0; found {} (PID 0x{product:04x}, bcdUSB 0x{revision:04x})",
        candidates.len()
    );
    let endpoint = candidates[0];
    ensure!(
        endpoint["attributes"] == 2 && endpoint["interface"].as_u64().is_some_and(|n| n <= 255),
        "camera USB endpoint 0x81 is not a valid bulk-IN interface: {endpoint}"
    );
    let link = match (revision, endpoint["maxPacketSize"].as_u64()) {
        (0x0200 | 0x0210, Some(512)) if endpoint.get("maxBurst").is_none() => Link::HighSpeed,
        (0x0300 | 0x0310 | 0x0320, Some(1024))
            if endpoint["maxBurst"].as_u64().is_some_and(|n| n <= 15) =>
        {
            Link::SuperSpeed
        }
        _ => anyhow::bail!(
            "unsupported camera USB transport: PID 0x{product:04x}, bcdUSB 0x{revision:04x}, endpoint {endpoint}; expected USB 2 high-speed/512 or USB 3 SuperSpeed/1024; full-speed USB is unsupported"
        ),
    };
    ensure!(
        product != 0x2209 || link == Link::HighSpeed,
        "ASI220MM Mini requires its USB 2 high-speed interface"
    );
    Ok(link)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn descriptor(pid: u32, revision: u16, packet: u16) -> Value {
        let mut info = json!({"vendorId":0x03c3,"productId":pid,"usbVersionBcd":revision,
            "endpoints":[{"interface":0,"alternate":0,"address":0x81,"attributes":2,"maxPacketSize":packet}]});
        if packet == 1024 {
            info["endpoints"][0]["maxBurst"] = json!(15);
        }
        info
    }

    #[test]
    fn every_supported_camera_accepts_usb2_and_usb3_models_keep_usb3() {
        for pid in [0x662b, 0x676d, 0x2601, 0x260e, 0x620b, 0x2209] {
            for revision in [0x200, 0x210] {
                assert_eq!(
                    validate(&descriptor(pid, revision, 512), pid).unwrap(),
                    Link::HighSpeed
                );
            }
            for revision in [0x300, 0x310, 0x320] {
                let result = validate(&descriptor(pid, revision, 1024), pid);
                if pid == 0x2209 {
                    assert!(result.is_err());
                } else {
                    assert_eq!(result.unwrap(), Link::SuperSpeed);
                }
            }
        }
    }

    #[test]
    fn bad_identity_speed_or_endpoint_never_passes_the_write_gate() {
        let good = descriptor(0x662b, 0x210, 512);
        assert!(validate(&good, 0x676d).is_err());
        for (field, value) in [
            ("vendorId", json!(0)),
            ("productId", json!(0)),
            ("usbVersionBcd", json!(0x110)),
            ("usbVersionBcd", Value::Null),
            ("endpoints", json!([])),
            ("endpoints", Value::Null),
        ] {
            let mut bad = good.clone();
            bad[field] = value;
            assert!(validate(&bad, 0x662b).is_err(), "{bad}");
        }
        for (field, value) in [
            ("address", json!(0x82)),
            ("attributes", json!(3)),
            ("alternate", json!(1)),
            ("interface", json!(256)),
            ("maxPacketSize", json!(64)),
            ("maxPacketSize", json!(1024)),
            ("maxBurst", json!(0)),
        ] {
            let mut bad = good.clone();
            bad["endpoints"][0][field] = value;
            assert!(validate(&bad, 0x662b).is_err(), "{bad}");
        }
        let mut duplicate = good.clone();
        duplicate["endpoints"]
            .as_array_mut()
            .unwrap()
            .push(good["endpoints"][0].clone());
        assert!(validate(&duplicate, 0x662b).is_err());
        let mut superspeed = descriptor(0x662b, 0x300, 1024);
        superspeed["endpoints"][0]["maxBurst"] = json!(16);
        assert!(validate(&superspeed, 0x662b).is_err());
        superspeed["endpoints"][0]
            .as_object_mut()
            .unwrap()
            .remove("maxBurst");
        assert!(validate(&superspeed, 0x662b).is_err());
        assert!(validate(&descriptor(0x662b, 0x300, 512), 0x662b).is_err());
    }

    #[test]
    fn failure_reports_observed_transport_without_serial_or_device_path() {
        let error = validate(&descriptor(0x662b, 0x200, 64), 0x662b)
            .unwrap_err()
            .to_string();
        assert!(error.contains("PID 0x662b"));
        assert!(error.contains("bcdUSB 0x0200"));
        assert!(error.contains("full-speed USB is unsupported"));
    }
}
