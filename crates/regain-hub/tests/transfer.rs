use regain_hub::{
    config::*,
    transfer::{self, ImportMode},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use uuid::Uuid;

fn config(text: &str) -> HubConfig {
    serde_json::from_str(text).unwrap()
}
fn saved(text: &str) -> HubConfig {
    ConfigStore::new(None, config(text)).unwrap().snapshot()
}
fn file(config: &HubConfig) -> String {
    serde_json::to_string(&transfer::export(config)).unwrap()
}
const SAFETY: &str = include_str!("../examples/two-source-safety.json");

#[test]
fn restore_preserves_destination_history_and_credentials_without_exporting_bindings() {
    let mut original = config(SAFETY);
    let id = original.sources[0].id;
    if let SourceBackend::Alpaca {
        credential_reference,
        ..
    } = &mut original.sources[0].backend
    {
        *credential_reference = Some("private-reference".into());
    }
    let store = ConfigStore::new(None, original).unwrap();
    let before = store.snapshot();
    let text = file(&before);
    assert!(!text.contains("private-reference"));
    let mut document: Value = serde_json::from_str(&text).unwrap();
    document["configuration"]["identities"] =
        json!({"sources":{},"outputs":{},"channels":{},"groups":[],"cameraGroups":[]});
    document["configuration"]["outputs"][0]["label"] = "Restored label".into();
    document["credentialSources"] = json!([]); // A backup without a binding must not erase a local one.
    let result = transfer::prepare(&before, &document.to_string(), ImportMode::Restore).unwrap();
    assert_eq!(result.preserved_credentials, vec![id]);
    assert!(result.missing_credentials.is_empty());
    assert_eq!(result.candidate.sources, before.sources);
    assert_eq!(result.candidate.identities, before.identities);
    assert_eq!(result.candidate.revision, before.revision);
    assert!(result.remapped_ids.is_empty());
    assert!(result.renumbered_outputs.is_empty());
    assert_eq!(store.snapshot(), before);
    let next = store
        .prepare(before.revision, result.candidate, false)
        .unwrap();
    store.commit(next).unwrap();
    assert_eq!(store.snapshot().outputs[0].label, "Restored label");
}

#[test]
fn missing_bindings_are_reported_with_destination_ids_for_restore_and_copy() {
    let before = saved(SAFETY);
    let mut document: Value = serde_json::from_str(&file(&before)).unwrap();
    document["credentialSources"] = json!([before.sources[0].id]);
    let restore = transfer::prepare(&before, &document.to_string(), ImportMode::Restore).unwrap();
    assert_eq!(restore.missing_credentials, vec![before.sources[0].id]);
    let copy = transfer::prepare(&before, &document.to_string(), ImportMode::Copy).unwrap();
    assert_eq!(copy.missing_credentials, vec![copy.candidate.sources[0].id]);
    assert!(copy.preserved_credentials.is_empty());
}

fn rewrite(value: &mut Value, ids: &BTreeMap<String, String>) {
    match value {
        Value::String(text) => {
            if let Some(next) = ids.get(text) {
                *text = next.clone();
            }
        }
        Value::Array(array) => {
            for value in array {
                rewrite(value, ids);
            }
        }
        Value::Object(object) => {
            for value in object.values_mut() {
                rewrite(value, ids);
            }
        }
        _ => {}
    }
}
#[test]
fn copy_rewrites_every_graph_reference_and_keeps_other_settings() {
    for text in [
        SAFETY,
        include_str!("../examples/mixed-switch.json"),
        include_str!("../examples/mixed-weather.json"),
        include_str!("../examples/paired-focusers.json"),
        include_str!("../examples/paired-cameras.json"),
    ] {
        let source = saved(text);
        let mut destination = source.clone();
        destination.instance_id = Uuid::new_v4();
        destination.revision = Uuid::new_v4();
        let store = ConfigStore::new(None, destination).unwrap();
        let destination = store.snapshot();
        assert!(transfer::prepare(&destination, &file(&source), ImportMode::Restore).is_err());
        let result = transfer::prepare(&destination, &file(&source), ImportMode::Copy).unwrap();
        assert_eq!(result.source_instance_id, source.instance_id);
        assert_eq!(result.candidate.instance_id, destination.instance_id);
        assert!(result.candidate.validate().is_empty());
        let ids: BTreeMap<_, _> = result
            .remapped_ids
            .iter()
            .map(|m| (m.original.to_string(), m.replacement.to_string()))
            .collect();
        assert!(ids.iter().all(|(a, b)| a != b && !ids.contains_key(b)));
        let mut expected = serde_json::to_value(source).unwrap();
        expected["identities"] = json!({});
        rewrite(&mut expected, &ids);
        expected["instanceId"] = json!(destination.instance_id);
        expected["revision"] = json!(destination.revision);
        expected["identities"] = serde_json::to_value(&destination.identities).unwrap();
        for number in &result.renumbered_outputs {
            let output = expected["outputs"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|o| o["id"] == number.output.to_string())
                .unwrap();
            assert_eq!(output["number"], number.original);
            output["number"] = number.replacement.into();
        }
        assert_eq!(expected, serde_json::to_value(&result.candidate).unwrap());
        let staged = store
            .prepare(destination.revision, result.candidate, false)
            .unwrap();
        store.commit(staged).unwrap();
    }
}

#[test]
fn retired_numbers_are_not_reused_and_imported_history_cannot_clear_them() {
    let store = ConfigStore::new(None, config(SAFETY)).unwrap();
    let original = store.snapshot();
    let mut empty = original.clone();
    empty.outputs.clear();
    empty.sources.clear();
    store
        .commit(store.prepare(original.revision, empty, false).unwrap())
        .unwrap();
    let destination = store.snapshot();
    let copied = transfer::prepare(&destination, &file(&original), ImportMode::Copy).unwrap();
    assert_eq!(copied.candidate.outputs[0].number, 1);
    assert_eq!(copied.candidate.identities, destination.identities);
    store
        .prepare(destination.revision, copied.candidate, false)
        .unwrap();
    let restored = transfer::prepare(&destination, &file(&original), ImportMode::Restore).unwrap();
    assert_eq!(restored.candidate.outputs[0].number, 0); // Same identity may be restored.
}

#[test]
fn invalid_files_do_not_bypass_identity_or_redaction_rules() {
    let before = saved(SAFETY);
    let document: Value = serde_json::from_str(&file(&before)).unwrap();
    for fault in [
        "version",
        "unknown",
        "duplicate-credentials",
        "unknown-credential",
        "non-alpaca-credential",
        "reference",
        "retarget",
        "renumber",
        "pin",
        "broken-reference",
    ] {
        let mut bad = document.clone();
        match fault {
            "version" => bad["formatVersion"] = 99.into(),
            "unknown" => bad["unknown"] = true.into(),
            "duplicate-credentials" => {
                bad["credentialSources"] = json!([before.sources[0].id, before.sources[0].id])
            }
            "unknown-credential" => bad["credentialSources"] = json!([Uuid::new_v4()]),
            "non-alpaca-credential" => {
                bad["configuration"]["sources"][0]["backend"] =
                    json!({"kind":"simulated","deviceType":"safetymonitor"});
                bad["credentialSources"] = json!([before.sources[0].id]);
            }
            "reference" => {
                bad["configuration"]["sources"][0]["backend"]["credentialReference"] =
                    "not-accepted".into()
            }
            "retarget" => bad["configuration"]["sources"][0]["backend"]["deviceNumber"] = 42.into(),
            "renumber" => bad["configuration"]["outputs"][0]["number"] = 7.into(),
            "pin" => {
                bad["configuration"]["sources"][0]["id"] = json!(before.outputs[0].id);
            }
            "broken-reference" => {
                bad["configuration"]["outputs"][0]["device"]["members"][0]["source"] =
                    json!(Uuid::new_v4())
            }
            _ => unreachable!(),
        }
        assert!(
            transfer::prepare(&before, &bad.to_string(), ImportMode::Restore).is_err(),
            "{fault}"
        );
    }
    let text = file(&before);
    for bad in [
        "{}".into(),
        "[]".into(),
        format!("{{\"formatVersion\":1,{}", &text[1..]),
        text.replace("\"sources\":{", "\"sources\":{\"x\":\"a\",\"x\":\"b\","),
        " ".repeat(MAX_CONFIG_BYTES + 1),
    ] {
        assert!(transfer::prepare(&before, &bad, ImportMode::Copy).is_err());
    }
    assert!(transfer::prepare(&before, &format!("\u{feff}{text}"), ImportMode::Restore).is_ok());
    let ledger_duplicate = text.replacen(
        "\"sources\":{",
        &format!("\"sources\":{{\"{}\":\"ignored\",", before.sources[0].id),
        1,
    );
    let weather = file(&saved(include_str!("../examples/mixed-weather.json")));
    let weather_value: Value = serde_json::from_str(&weather).unwrap();
    let measurement =
        &weather_value["configuration"]["outputs"][0]["device"]["measurements"]["temperature"];
    let weather_duplicate = weather.replacen(
        "\"temperature\":",
        &format!("\"temperature\":{measurement},\"temperature\":"),
        1,
    );
    for duplicate in [ledger_duplicate, weather_duplicate] {
        assert!(
            serde_json::from_str::<transfer::Document>(&duplicate).is_ok(),
            "both repeated values must themselves be valid"
        );
        assert!(transfer::prepare(&before, &duplicate, ImportMode::Copy).is_err());
    }
}

#[test]
fn an_embedded_runtime_without_staged_history_still_protects_its_active_ids() {
    let current = config(SAFETY);
    let mut document: Value = serde_json::from_str(&file(&current)).unwrap();
    document["configuration"]["sources"][0]["backend"]["deviceNumber"] = 42.into();
    assert!(transfer::prepare(&current, &document.to_string(), ImportMode::Restore).is_err());
}
