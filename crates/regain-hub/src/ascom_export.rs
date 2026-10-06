//! Stable native ASCOM output identities, also used to reject self-proxies.
use crate::config::{DeviceType, HubConfig};
use uuid::Uuid;

pub fn class_id(instance: Uuid, output: Uuid, device: DeviceType) -> Uuid {
    let kind = serde_json::to_value(device).expect("Device type serializes");
    Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!(
            "https://pulsarfab.com/regain/ascom-hub/output/{instance}/{output}/{}",
            kind.as_str().unwrap()
        )
        .as_bytes(),
    )
}
pub fn prog_id(instance: Uuid, output: Uuid, device: DeviceType) -> Option<String> {
    let prefix = match device {
        DeviceType::Switch => "Rgn.HS.",
        DeviceType::SafetyMonitor => "Rgn.HM.",
        DeviceType::ObservingConditions => "Rgn.HW.",
        DeviceType::Focuser => "Rgn.HF.",
        DeviceType::Rotator => "Rgn.HR.",
        _ => return None,
    };
    Some(format!(
        "{prefix}{}",
        class_id(instance, output, device).simple()
    ))
}
pub(crate) fn classes(config: &HubConfig) -> Vec<Uuid> {
    config
        .outputs
        .iter()
        .map(|output| class_id(config.instance_id, output.id, output.device.device_type()))
        .collect()
}
