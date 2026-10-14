//! Explicit local catalogs. Native identity probing runs in existing owned
//! workers; COM reads registrations without activating drivers. No source lease,
//! motion, acquisition, saved configuration or implicit output is created.
use crate::{
    camera::config::{CameraRecovery, NativeCameraConfig},
    config::{Bitness, ConnectionPolicy, DeviceType, NativeDevice, SourceBackend},
    native::NativeRuntime,
    source::{ErrorKind, SourceError},
};
use regain_core::accessory::AccessoryWorker;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeSet, process::Stdio, time::Duration};
use tokio::process::Command;
use uuid::Uuid;

pub const TIMEOUT: Duration = Duration::from_secs(20);
pub const MAX_ENTRIES: usize = 256;
#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Target {
    Native {
        device: NativeDevice,
    },
    Com {
        device_type: DeviceType,
        bitness: Bitness,
    },
}
#[derive(Clone, Copy, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum BlockedReason {
    MissingRegistration,
    SelfProxy,
}
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Entry {
    #[schemars(length(min = 1, max = 200))]
    pub name: String,
    pub backend: SourceBackend,
    pub registered_class: Option<Uuid>,
    pub blocked_reason: Option<BlockedReason>,
}
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Catalog {
    pub configuration_revision: Uuid,
    pub target: Target,
    pub simulated: bool,
    #[schemars(length(max = 256))]
    pub entries: Vec<Entry>,
    #[schemars(range(min = 0, max = 256))]
    pub ignored_entries: u32,
    pub incomplete: bool,
}
pub fn description() -> Value {
    json!({"operation":"discoverLocal", "persistsConfiguration":false,"opensSource":false,
        "writesEquipment":false,"timeoutSeconds":TIMEOUT.as_secs(),"maximumEntries":MAX_ENTRIES,
        "responseSchema":schemars::schema_for!(Catalog),"targetSchema":schemars::schema_for!(Target),
        "native":{"label":"Find native Regain devices","description":"Disconnect all hub outputs and wait for transports to close. Briefly open matching devices to read identity using the selected backend. Other applications may make devices unavailable. No motion or exposure is requested.","requiresCapability":"nativeSources","requiresDisconnect":true,"probesIdentity":true},
        "com":{"label":"Find installed ASCOM drivers","description":"Read the selected architecture's registrations on the shared host without activating drivers. Select an entry to add a draft source; connection and setup remain separate.","requiresCapability":"comSources","activatesDrivers":false},
        "clientCancellation":"The finite owned catalog job finishes or times out even if its client disconnects. It is never retried automatically.",
        "blockedReasons":{"missingRegistration":"The selected architecture has no valid class registration.","selfProxy":"This registration points back to an output of this hub."}})
}
fn failure() -> SourceError {
    SourceError::new(
        ErrorKind::Unavailable,
        "Local discovery failed or returned an invalid catalog; no probe was retried",
    )
}
fn clean(value: &str) -> bool {
    !value.trim().is_empty() && value.chars().count() <= 200 && !value.chars().any(char::is_control)
}
fn worker_command(runtime: &NativeRuntime, target: Target) -> Result<Command, SourceError> {
    let mut command = match target {
        Target::Native { device } => {
            let (vendor, device) = crate::native::worker_arguments(device)?;
            let mut command = Command::new(
                runtime
                    .directory
                    .join(format!("regain-device{}", std::env::consts::EXE_SUFFIX)),
            );
            command.args(["--hub-discover", vendor, device]);
            if runtime.simulate {
                command.arg("--simulate");
            }
            command
        }
        Target::Com {
            device_type,
            bitness,
        } => {
            if !crate::com::available_architectures(runtime).contains(&bitness) {
                return Err(SourceError::new(
                    ErrorKind::Unsupported,
                    "The selected COM discovery worker is unavailable",
                ));
            }
            let architecture = if bitness == Bitness::X86 {
                "x86"
            } else {
                "x64"
            };
            let mut command = Command::new(
                runtime
                    .directory
                    .join("hub-ascom")
                    .join(architecture)
                    .join("Regain.Hub.ASCOM.exe"),
            );
            command.args([
                "--discover",
                "--device-type",
                serde_json::to_value(device_type).unwrap().as_str().unwrap(),
                "--bitness",
                architecture,
            ]);
            command
        }
    };
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    Ok(command)
}
#[derive(Deserialize)]
struct Identity {
    serial: String,
    model: Option<String>,
}
#[derive(Deserialize)]
struct AccessoryEntry {
    identity: Option<Identity>,
    serial: Option<String>,
    model: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ComEntry {
    name: String,
    prog_id: String,
    class_id: Option<Uuid>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ComCatalog {
    entries: Vec<ComEntry>,
    incomplete: bool,
}
fn native_entry(
    device: NativeDevice,
    serial: String,
    name: Option<String>,
) -> Result<Entry, SourceError> {
    let name = name.unwrap_or_else(|| {
        match device {
            NativeDevice::Caa => "ZWO CAA",
            NativeDevice::Efw => "ZWO EFW",
            NativeDevice::Eaf => "ZWO EAF",
            NativeDevice::Fc3 => "Pegasus Astro FocusCube3",
            NativeDevice::Falcon => "Pegasus Astro Falcon V2",
            NativeDevice::Ofp2 => "Deep Sky Dad OFP2",
            NativeDevice::Eta => "Wanderer Astro ETA M54",
            NativeDevice::CameraDirect | NativeDevice::CameraSdk => "",
        }
        .into()
    });
    if !clean(&serial) || !clean(&name) {
        return Err(failure());
    }
    let camera =
        matches!(device, NativeDevice::CameraDirect | NativeDevice::CameraSdk).then(|| {
            NativeCameraConfig {
                model: name.clone(),
                sdk_fallback: false,
                recovery: CameraRecovery::default(),
            }
        });
    Ok(Entry {
        name,
        backend: SourceBackend::Native {
            device,
            identity: serial.to_ascii_lowercase(),
            filter_wheel: None,
            temperature_compensation: None,
            camera,
        },
        registered_class: None,
        blocked_reason: None,
    })
}
pub async fn discover(
    runtime: NativeRuntime,
    target: Target,
    revision: Uuid,
    denied: Vec<Uuid>,
) -> Result<Catalog, SourceError> {
    if revision.is_nil() {
        return Err(failure());
    }
    let mut catalog = Catalog {
        configuration_revision: revision,
        target,
        simulated: matches!(target, Target::Native { .. }) && runtime.simulate,
        entries: vec![],
        ignored_entries: 0,
        incomplete: false,
    };
    match target {
        Target::Native {
            device: NativeDevice::CameraDirect | NativeDevice::CameraSdk,
        } => {
            let cameras = runtime.cameras.as_ref().ok_or_else(|| {
                SourceError::new(
                    ErrorKind::Unsupported,
                    "Native camera discovery is unavailable",
                )
            })?;
            let core = regain_core::Runtime {
                directory: runtime.directory.clone(),
                sdk: cameras.sdk.clone(),
                simulate: runtime.simulate,
                sdk_simulation: cameras.sdk_simulation.clone(),
            };
            let direct = matches!(
                target,
                Target::Native {
                    device: NativeDevice::CameraDirect
                }
            );
            let mut worker = core
                .spawn(direct, std::sync::Arc::new(|_, _, _| {}))
                .await
                .map_err(|_| failure())?;
            let result = worker
                .call_json(
                    "list",
                    json!({"serials":true}),
                    TIMEOUT.as_secs_f64(),
                    &regain_core::CancellationToken::new(),
                )
                .await;
            worker.kill().await;
            let rows = result.map_err(|_| failure())?;
            let rows = rows.as_array().ok_or_else(failure)?;
            if rows.len() > MAX_ENTRIES {
                return Err(failure());
            }
            for row in rows {
                if let (Some(serial), Some(name)) = (row["serial"].as_str(), row["name"].as_str()) {
                    catalog.entries.push(native_entry(
                        if direct {
                            NativeDevice::CameraDirect
                        } else {
                            NativeDevice::CameraSdk
                        },
                        serial.into(),
                        Some(name.into()),
                    )?);
                } else {
                    catalog.ignored_entries += 1;
                }
            }
        }
        Target::Native { device } => {
            let child = worker_command(&runtime, target)?
                .spawn()
                .map_err(|_| failure())?;
            let collected = AccessoryWorker::new(child)
                .map_err(|_| failure())?
                .collect_json_with_timeout::<Vec<AccessoryEntry>>(
                    json!({"command":"discover"}),
                    TIMEOUT,
                )
                .await
                .map_err(|_| failure())?;
            if collected.value.len() > MAX_ENTRIES {
                return Err(failure());
            }
            catalog.incomplete = collected.diagnostics_present;
            for row in collected.value {
                let identity = if device == NativeDevice::Ofp2 {
                    row.serial.map(|serial| Identity {
                        serial,
                        model: row.model,
                    })
                } else {
                    row.identity
                };
                if let Some(identity) = identity {
                    catalog
                        .entries
                        .push(native_entry(device, identity.serial, identity.model)?);
                } else {
                    catalog.ignored_entries += 1;
                }
            }
        }
        Target::Com {
            device_type,
            bitness,
        } => {
            let child = worker_command(&runtime, target)?
                .spawn()
                .map_err(|_| failure())?;
            let collected = AccessoryWorker::new_independent(child)
                .map_err(|_| failure())?
                .collect_json_with_timeout::<ComCatalog>(json!({"command":"discover"}), TIMEOUT)
                .await
                .map_err(|_| failure())?;
            if collected.value.entries.len() > MAX_ENTRIES {
                return Err(failure());
            }
            catalog.incomplete = collected.value.incomplete || collected.diagnostics_present;
            for row in collected.value.entries {
                if !clean(&row.name)
                    || !clean(&row.prog_id)
                    || !row
                        .prog_id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                    || row.class_id == Some(Uuid::nil())
                {
                    return Err(failure());
                }
                let blocked_reason = if row.class_id.is_none() {
                    Some(BlockedReason::MissingRegistration)
                } else if row.class_id.is_some_and(|id| denied.contains(&id)) {
                    Some(BlockedReason::SelfProxy)
                } else {
                    None
                };
                catalog.entries.push(Entry {
                    name: row.name,
                    backend: SourceBackend::Com {
                        prog_id: row.prog_id,
                        device_type,
                        bitness,
                        connection_policy: ConnectionPolicy::ExternallyManaged,
                    },
                    registered_class: row.class_id,
                    blocked_reason,
                });
            }
        }
    }
    catalog.incomplete |= catalog.ignored_entries != 0;
    let mut identities = BTreeSet::new();
    for entry in &catalog.entries {
        let identity = match &entry.backend {
            // The direct simulator lists alternative camera models for one
            // simulated serial. Real devices must always have unique serials.
            SourceBackend::Native {
                device: NativeDevice::CameraDirect,
                identity,
                camera: Some(camera),
                ..
            } if catalog.simulated => format!("{}:{}", identity.to_ascii_lowercase(), camera.model),
            SourceBackend::Native { identity, .. } => identity.to_ascii_lowercase(),
            SourceBackend::Com { prog_id, .. } => prog_id.to_ascii_lowercase(),
            _ => return Err(failure()),
        };
        if !identities.insert(identity) {
            return Err(failure());
        }
    }
    catalog.entries.sort_by(|a, b| {
        a.name.cmp(&b.name).then_with(|| {
            serde_json::to_string(&a.backend)
                .unwrap()
                .cmp(&serde_json::to_string(&b.backend).unwrap())
        })
    });
    Ok(catalog)
}
