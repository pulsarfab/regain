//! One runtime description for every setup frontend. JSON Schema encodes tagged
//! choices/conditional fields; annotations carry labels, units and capability gates.
use crate::config::{HubConfig, SCHEMA_VERSION};
use serde_json::{Value, json};

pub const CONTRACT_VERSION: u32 = 1;

pub fn describe_config(capabilities: &[&str]) -> Value {
    let mut schema = serde_json::to_value(schemars::schema_for!(HubConfig)).unwrap();
    decorate(&mut schema);
    json!({
        "contractVersion": CONTRACT_VERSION,
        "schemaVersion": SCHEMA_VERSION,
        "schema": schema,
        "capabilities": capabilities,
        "simulationControl": crate::simulated::description(),
        "outputDiagnostics": crate::diagnostics::description(),
        "coordination": {
            "cameraGroups": {
                "responseSchema": schemars::schema_for!(crate::coordination::HostedCameraStatus),
                "configurationKey": "cameraGroups",
                "startOperation": "startCameraGroup",
                "statusOperation": "cameraGroupStatus",
                "cancelOperation": "cancelCameraGroup",
                "imageOperation": "cameraGroupImage",
                "statusOpensSources": false,
                "imageOpensSources": false,
                "cancellationPolicy": "savedGroupPolicy",
                "retention": "latestPerGroupUntilHostOrRevisionChanges",
                "startSkew": "monotonicHostRequestSpreadNotSensorSynchronization",
                "images": "separateImmutablePinsUsingSharedImageBudget"
            },
            "focuserGroups": {
                "responseSchema": schemars::schema_for!(crate::coordination::HostedFocuserStatus),
                "configurationKey": "focuserGroups",
                "startOperation": "startFocuserGroup",
                "statusOperation": "focuserGroupStatus",
                "cancelOperation": "cancelFocuserGroup",
                "statusOpensSources": false,
                "cancelHaltsEquipment": false,
                "retention": "latestPerGroupUntilHostOrRevisionChanges",
                "target": {
                    "type": "integer", "format": "int32",
                    "minimum": i32::MIN, "maximum": i32::MAX,
                    "label": "Logical target", "units": "steps",
                    "description": "Logical group coordinate. Saved group bounds and each calibrated device target are checked before connecting sources."
                }
            }
        },
        "discovery": { "alpaca": crate::alpaca::discovery::description(),
            "network": crate::alpaca::network_discovery::description() },
        "apply": "disconnect",
        "validation": "The hub validates relationships, identities, capabilities and revisions before applying. Schema validation alone does not authorize an update."
    })
}

fn title(key: &str) -> String {
    let mut text = String::new();
    for (i, c) in key.chars().enumerate() {
        if i == 0 {
            text.extend(c.to_uppercase());
        } else {
            if c.is_uppercase() {
                text.push(' ');
            }
            text.push(c);
        }
    }
    text
}

fn decorate(node: &mut Value) {
    match node {
        Value::Object(map) => {
            if let Some(Value::Object(properties)) = map.get_mut("properties") {
                for (key, value) in properties {
                    if let Some(property) = value.as_object_mut() {
                        property.entry("title").or_insert_with(|| title(key).into());
                        // Applied only to schema nodes; never rewrite a user's defaults.
                        property
                            .entry("x-regain")
                            .or_insert_with(|| json!({"apply":"reconnect"}));
                    }
                }
            }
            for (key, value) in map {
                if !matches!(key.as_str(), "default" | "examples" | "x-regain") {
                    decorate(value);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                decorate(value);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parameters::{ParameterSet, SafetyPolicy};
    #[test]
    fn types_policies_and_ui_use_identical_definitions() {
        let description = describe_config(&[]);
        assert_eq!(description["contractVersion"], 1);
        let schema = &description["schema"];
        assert_eq!(
            schema["properties"]["schemaVersion"]["const"],
            SCHEMA_VERSION
        );
        assert_eq!(schema["properties"]["identities"]["readOnly"], true);
        for parameter in SafetyPolicy::parameters() {
            let field = &schema["$defs"]["SafetyPolicy"]["properties"][&parameter.key];
            assert_eq!(field["default"], parameter.default);
            assert_eq!(field["description"], parameter.description);
            assert_eq!(field["minimum"], parameter.minimum);
        }
        let variants = schema["$defs"]["SourceBackend"]["oneOf"]
            .as_array()
            .unwrap();
        let com = variants
            .iter()
            .find(|s| s["properties"]["kind"]["const"] == "com")
            .unwrap();
        assert_eq!(com["x-regain"]["requiresCapability"], "comSources");
        assert!(
            com["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == "bitness")
        );
    }
}
