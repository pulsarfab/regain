//! Redacted configuration files and inert, revision-owned import preparation.
//! Identity history belongs to the destination store, never to an imported file.
use crate::{
    config::{DeviceType, HubConfig, MAX_CONFIG_BYTES, Readout, SourceBackend, VirtualDevice},
    parameters::FieldError,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

pub const FORMAT_VERSION: u32 = 1;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ImportMode {
    Restore,
    Copy,
}
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Document {
    #[schemars(extend("const" = FORMAT_VERSION))]
    pub format_version: u32,
    pub configuration: HubConfig,
    /// Source IDs whose credential binding was omitted. Never reference values.
    #[schemars(length(max = 256))]
    pub credential_sources: Vec<Uuid>,
}
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdMapping {
    pub original: Uuid,
    pub replacement: Uuid,
}
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NumberMapping {
    pub output: Uuid,
    pub device_type: DeviceType,
    pub original: u32,
    pub replacement: u32,
}
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreparedImport {
    pub configuration_revision: Uuid,
    pub mode: ImportMode,
    pub source_instance_id: Uuid,
    pub candidate: HubConfig,
    pub remapped_ids: Vec<IdMapping>,
    pub renumbered_outputs: Vec<NumberMapping>,
    pub preserved_credentials: Vec<Uuid>,
    pub missing_credentials: Vec<Uuid>,
}
pub fn description() -> Value {
    json!({"exportOperation":"exportConfig","importOperation":"prepareImport",
        "formatVersion":FORMAT_VERSION,"maximumDocumentBytes":MAX_CONFIG_BYTES,
        "maximumFrameBytes":crate::ipc::MAX_FRAME_BYTES,"persistsConfiguration":false,"opensSource":false,"writesEquipment":false,
        "documentSchema":schemars::schema_for!(Document),"responseSchema":schemars::schema_for!(PreparedImport),
        "modes":[{"value":"restore","label":"Restore this hub","description":"Replace the draft using this hub's saved IDs. Keep destination identity history and matching existing credentials. Changes still require review and Apply."},
            {"value":"copy","label":"Copy settings with new identities","description":"Replace the draft with copied settings. Generate new IDs and unused device numbers while retaining destination history. Review hardware addresses and supply omitted credentials before Apply."}],
        "exportLabel":"Export saved configuration","importLabel":"Prepare imported draft",
        "simulationNotice":"SIMULATION — the saved configuration uses simulated sources.",
        "redaction":"Files omit credential references and secret values. Credential source IDs identify bindings to review. Equipment addresses and serials remain in the file.",
        "limits":"The existing IPC and HTTP frame limit also applies to encoded requests and responses; oversized files fail before changing the draft or saved configuration.",
        "review":"Import replaces only the draft. Review all settings, identity/number changes and credential bindings, then Apply separately."})
}
fn invalid(message: &str) -> Vec<FieldError> {
    vec![FieldError::new("document", "import", message)]
}
pub fn export(current: &HubConfig) -> Document {
    Document {
        format_version: FORMAT_VERSION,
        configuration: current.export(),
        credential_sources: current
            .sources
            .iter()
            .filter_map(|source| {
                matches!(
                    &source.backend,
                    SourceBackend::Alpaca {
                        credential_reference: Some(_),
                        ..
                    }
                )
                .then_some(source.id)
            })
            .collect(),
    }
}
fn map_readout(readout: &mut Readout, ids: &BTreeMap<Uuid, Uuid>) {
    match readout {
        Readout::Channel { source, .. } | Readout::Property { source, .. } => *source = ids[source],
    }
}
pub fn prepare(
    current: &HubConfig,
    text: &str,
    mode: ImportMode,
) -> Result<PreparedImport, Vec<FieldError>> {
    if text.len() > MAX_CONFIG_BYTES {
        return Err(invalid("Configuration file exceeds the document limit"));
    }
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    serde_json::from_str::<UniqueJson>(text)
        .map_err(|_| invalid("Invalid configuration file or duplicate object keys"))?;
    let document: Document = serde_json::from_str(text)
        .map_err(|_| invalid("Invalid configuration file or unsupported fields"))?;
    if document.format_version != FORMAT_VERSION {
        return Err(invalid("Unsupported configuration file version"));
    }
    let mut candidate = document.configuration;
    let errors = candidate.validate();
    if !errors.is_empty() {
        return Err(errors);
    }
    let required: BTreeSet<_> = document.credential_sources.iter().copied().collect();
    if required.len() != document.credential_sources.len()
        || required.len() > 256
        || required.iter().any(|id| {
            !candidate.sources.iter().any(|source| {
                source.id == *id && matches!(source.backend, SourceBackend::Alpaca { .. })
            })
        })
        || candidate.sources.iter().any(|source| {
            matches!(
                &source.backend,
                SourceBackend::Alpaca {
                    credential_reference: Some(_),
                    ..
                }
            )
        })
    {
        return Err(invalid(
            "Imported credential bindings must be omitted and identified only by source ID",
        ));
    }
    if mode == ImportMode::Restore && candidate.instance_id != current.instance_id {
        return Err(invalid(
            "Restore requires a file from this hub. Choose Copy for another hub",
        ));
    }
    let source_instance_id = candidate.instance_id;
    let mut remapped_ids = vec![];
    let mut renumbered_outputs = vec![];
    let mut missing_credentials = vec![];
    let mut preserved_credentials = vec![];
    if mode == ImportMode::Copy {
        let mut ids = BTreeMap::new();
        for id in candidate
            .sources
            .iter()
            .map(|s| s.id)
            .chain(candidate.outputs.iter().map(|o| o.id))
            .chain(candidate.outputs.iter().flat_map(|o| match &o.device {
                VirtualDevice::Switch { channels } => {
                    channels.iter().map(|c| c.id).collect::<Vec<_>>()
                }
                _ => vec![],
            }))
            .chain(candidate.focuser_groups.iter().map(|g| g.id))
            .chain(candidate.camera_groups.iter().map(|g| g.id))
        {
            ids.insert(id, Uuid::new_v4());
        }
        for source in &mut candidate.sources {
            if required.contains(&source.id) {
                missing_credentials.push(ids[&source.id]);
            }
            source.id = ids[&source.id];
            if let SourceBackend::Virtual { output } = &mut source.backend {
                *output = ids[output];
            }
        }
        for output in &mut candidate.outputs {
            output.id = ids[&output.id];
            match &mut output.device {
                VirtualDevice::Switch { channels } => {
                    for channel in channels {
                        channel.id = ids[&channel.id];
                        map_readout(&mut channel.readout, &ids);
                    }
                }
                VirtualDevice::Safety { members } => {
                    for member in members {
                        member.source = ids[&member.source];
                    }
                }
                VirtualDevice::Weather { measurements } => {
                    for measurement in measurements.values_mut() {
                        for readout in &mut measurement.sources {
                            map_readout(readout, &ids);
                        }
                    }
                }
                VirtualDevice::Proxy { source, .. } => *source = ids[source],
            }
        }
        for group in &mut candidate.focuser_groups {
            group.id = ids[&group.id];
            for member in &mut group.members {
                member.source = ids[&member.source];
            }
        }
        for group in &mut candidate.camera_groups {
            group.id = ids[&group.id];
            for member in &mut group.members {
                *member = ids[member];
            }
        }
        let mut reserved = current.identities.output_numbers();
        reserved.extend(
            current
                .outputs
                .iter()
                .map(|output| (output.device.device_type(), output.number)),
        );
        for output in &mut candidate.outputs {
            let device_type = output.device.device_type();
            let original = output.number;
            if reserved.contains(&(device_type, output.number)) {
                output.number = 0;
                while reserved.contains(&(device_type, output.number)) {
                    output.number = output
                        .number
                        .checked_add(1)
                        .ok_or_else(|| invalid("No unused output number is available"))?;
                }
                renumbered_outputs.push(NumberMapping {
                    output: output.id,
                    device_type,
                    original,
                    replacement: output.number,
                });
            }
            reserved.insert((device_type, output.number));
        }
        remapped_ids = ids
            .into_iter()
            .map(|(original, replacement)| IdMapping {
                original,
                replacement,
            })
            .collect();
    } else {
        for source in &mut candidate.sources {
            let existing = current.sources.iter().find(|entry| entry.id == source.id);
            let reference = existing.and_then(|entry| match &entry.backend {
                SourceBackend::Alpaca {
                    credential_reference,
                    ..
                } => credential_reference.clone(),
                _ => None,
            });
            if let SourceBackend::Alpaca {
                credential_reference,
                ..
            } = &mut source.backend
            {
                *credential_reference = reference;
            }
            if matches!(
                &source.backend,
                SourceBackend::Alpaca {
                    credential_reference: Some(_),
                    ..
                }
            ) {
                preserved_credentials.push(source.id);
            } else if required.contains(&source.id) {
                missing_credentials.push(source.id);
            }
        }
    }
    candidate.instance_id = current.instance_id;
    candidate.revision = current.revision;
    candidate.identities = current.identities.clone();
    let mut errors = crate::factory::source_plans(&candidate)
        .err()
        .unwrap_or_default();
    let mut ledger = current.identities.clone();
    // Embedded read-only runtimes can have an unstaged/default ledger. Protect
    // their active IDs too, without publishing a reconstructed history.
    errors.extend(ledger.register(
        &current.sources,
        &current.outputs,
        &current.focuser_groups,
        &current.camera_groups,
    ));
    errors.extend(ledger.register(
        &candidate.sources,
        &candidate.outputs,
        &candidate.focuser_groups,
        &candidate.camera_groups,
    ));
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(PreparedImport {
        configuration_revision: current.revision,
        mode,
        source_instance_id,
        candidate,
        remapped_ids,
        renumbered_outputs,
        preserved_credentials,
        missing_credentials,
    })
}

// Validate every object before typed deserialization: serde's map containers
// otherwise silently accept repeated weather keys or identity-ledger entries.
struct UniqueJson;
impl<'de> Deserialize<'de> for UniqueJson {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = UniqueJson;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("JSON with unique object keys")
            }
            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<UniqueJson, E> {
                Ok(UniqueJson)
            }
            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<UniqueJson, E> {
                Ok(UniqueJson)
            }
            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<UniqueJson, E> {
                Ok(UniqueJson)
            }
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<UniqueJson, E> {
                Ok(UniqueJson)
            }
            fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<UniqueJson, E> {
                Ok(UniqueJson)
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<UniqueJson, E> {
                Ok(UniqueJson)
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<UniqueJson, A::Error> {
                while sequence.next_element::<UniqueJson>()?.is_some() {}
                Ok(UniqueJson)
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<UniqueJson, A::Error> {
                let mut keys = BTreeSet::new();
                while let Some(key) = map.next_key::<String>()? {
                    if !keys.insert(key) {
                        return Err(serde::de::Error::custom("duplicate object key"));
                    }
                    map.next_value::<UniqueJson>()?;
                }
                Ok(UniqueJson)
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}
