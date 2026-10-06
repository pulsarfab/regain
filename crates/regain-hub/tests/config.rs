use regain_hub::config::*;
use regain_hub::parameters::PollPolicy;
use std::sync::{Arc, Barrier};
use uuid::Uuid;

fn safety() -> HubConfig {
    serde_json::from_str(include_str!("../examples/two-source-safety.json")).unwrap()
}
fn switches() -> HubConfig {
    serde_json::from_str(include_str!("../examples/mixed-switch.json")).unwrap()
}
fn invalid(config: &HubConfig, code: &str) {
    assert!(
        config.validate().iter().any(|e| e.code == code),
        "expected {code}: {:?}",
        config.validate()
    );
}
#[test]
fn initialization_publishes_one_empty_identity_and_never_overwrites() {
    let directory = tempfile::tempdir().unwrap();
    let path = Arc::new(directory.path().join("hub.json"));
    let barrier = Arc::new(Barrier::new(8));
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                ConfigStore::create(&path)
            })
        })
        .collect();
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    let successful: Vec<_> = results
        .iter()
        .filter_map(|result| result.as_ref().ok())
        .collect();
    assert_eq!(successful.len(), 1);
    let saved = ConfigStore::load(&path).unwrap().snapshot();
    assert_eq!(successful[0].snapshot(), saved);
    assert!(saved.sources.is_empty() && saved.outputs.is_empty());
    assert!(!saved.instance_id.is_nil() && !saved.revision.is_nil());
    let bytes = std::fs::read(&*path).unwrap();
    assert!(ConfigStore::create(&path).is_err());
    assert_eq!(std::fs::read(&*path).unwrap(), bytes);
    // Initialization's store uses the same durable editing path as a loaded one.
    let created = successful[0];
    let changed = created.apply(saved.revision, saved.clone(), false).unwrap();
    assert_ne!(changed.revision, saved.revision);
    assert_eq!(changed.instance_id, saved.instance_id);
    assert_eq!(ConfigStore::load(&path).unwrap().snapshot(), changed);
}

#[test]
fn initialization_preserves_invalid_documents_and_requires_an_existing_parent() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("invalid.json");
    std::fs::write(&path, b"keep invalid data intact").unwrap();
    assert!(ConfigStore::create(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"keep invalid data intact");
    assert!(ConfigStore::create(directory.path()).is_err());
    assert!(ConfigStore::create(&directory.path().join("missing/hub.json")).is_err());
    assert!(!directory.path().join("missing").exists());
    assert!(ConfigStore::create(std::path::Path::new("relative-hub.json")).is_err());
    #[cfg(unix)]
    {
        let alias = directory.path().join("alias.json");
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        assert!(ConfigStore::create(&alias).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"keep invalid data intact");
    }
}
#[test]
fn native_ascom_export_identities_match_cross_language_vectors_and_reject_canonical_self_proxies() {
    use regain_hub::ascom_export::{class_id, prog_id};
    let instance = "10000000-0000-0000-0000-000000000001".parse().unwrap();
    let output = "20000000-0000-0000-0000-000000000002".parse().unwrap();
    for (device, prefix, expected) in [
        (
            DeviceType::Switch,
            "Rgn.HS.",
            "69a5917f-8d71-5a9d-b3e7-8d5a53f88e0b",
        ),
        (
            DeviceType::SafetyMonitor,
            "Rgn.HM.",
            "14421c22-3804-5450-91c1-211547b06944",
        ),
        (
            DeviceType::ObservingConditions,
            "Rgn.HW.",
            "f3d3f0d7-9c8d-5b4d-834c-04b0805a56ff",
        ),
        (
            DeviceType::Focuser,
            "Rgn.HF.",
            "e8862a89-95df-5b67-8555-142cfcdc810f",
        ),
        (
            DeviceType::Rotator,
            "Rgn.HR.",
            "7c9d3910-2aa2-5ef1-addd-f2db0c7dd14f",
        ),
    ] {
        assert_eq!(class_id(instance, output, device).to_string(), expected);
        assert_eq!(
            prog_id(instance, output, device).unwrap(),
            format!("{prefix}{}", expected.replace('-', ""))
        );
    }
    let mut config = safety();
    let own = prog_id(
        config.instance_id,
        config.outputs[0].id,
        DeviceType::SafetyMonitor,
    )
    .unwrap();
    config.sources[0].backend = SourceBackend::Com {
        prog_id: own.to_uppercase(),
        device_type: DeviceType::SafetyMonitor,
        bitness: Bitness::X64,
        connection_policy: ConnectionPolicy::Managed,
    };
    invalid(&config, "cycle");
    config.outputs[0].label = "Renamed".into();
    config.outputs.reverse();
    invalid(&config, "cycle");
    if let SourceBackend::Com {
        prog_id: source, ..
    } = &mut config.sources[0].backend
    {
        *source = prog_id(
            Uuid::new_v4(),
            config.outputs[0].id,
            DeviceType::SafetyMonitor,
        )
        .unwrap();
    }
    assert!(
        config.validate().is_empty(),
        "A different hub instance is not a local self-proxy"
    );
    for device_type in [DeviceType::Focuser, DeviceType::Rotator] {
        let source = Uuid::new_v4();
        let mut typed = HubConfig::empty();
        typed.outputs.push(OutputConfig {
            id: output,
            number: 42,
            label: "Typed output".into(),
            device: VirtualDevice::Proxy {
                source,
                device_type,
            },
        });
        typed.sources.push(SourceConfig {
            id: source,
            label: "Typed self-proxy".into(),
            polling: PollPolicy::default(),
            backend: SourceBackend::Com {
                prog_id: prog_id(typed.instance_id, output, device_type)
                    .unwrap()
                    .to_uppercase(),
                device_type,
                bitness: Bitness::X64,
                connection_policy: ConnectionPolicy::Managed,
            },
        });
        invalid(&typed, "cycle");
        typed.outputs[0].label = "Renamed typed output".into();
        invalid(&typed, "cycle");
        if let SourceBackend::Com { prog_id: value, .. } = &mut typed.sources[0].backend {
            *value = prog_id(Uuid::new_v4(), output, device_type).unwrap();
        }
        assert!(typed.validate().is_empty());
    }
}

#[test]
fn examples_validate_and_preserve_ids_on_roundtrip_and_reorder() {
    for config in [
        safety(),
        switches(),
        serde_json::from_str(include_str!("../examples/mixed-weather.json")).unwrap(),
        serde_json::from_str(include_str!("../examples/simulated-observatory.json")).unwrap(),
    ] {
        assert!(config.validate().is_empty(), "{:?}", config.validate());
        let store = ConfigStore::new(None, config).unwrap();
        let before = store.snapshot();
        let mut next = before.clone();
        next.sources.reverse();
        next.outputs.reverse();
        next.outputs[0].label = "Renamed".into();
        let after = store.apply(before.revision, next, false).unwrap();
        assert_ne!(before.revision, after.revision);
        assert_eq!(before.identities, after.identities);
        let roundtrip: HubConfig =
            serde_json::from_slice(&serde_json::to_vec(&after).unwrap()).unwrap();
        assert_eq!(roundtrip, after);
    }
}
#[test]
fn malformed_versions_and_unknown_fields_are_not_silently_migrated() {
    let mut config = safety();
    config.schema_version = 2;
    invalid(&config, "version");
    let mut value = serde_json::to_value(safety()).unwrap();
    value.as_object_mut().unwrap().remove("schemaVersion");
    assert!(serde_json::from_value::<HubConfig>(value).is_err());
    let mut value = serde_json::to_value(safety()).unwrap();
    value["sources"][0]["backend"]["password"] = "must-not-accept".into();
    assert!(serde_json::from_value::<HubConfig>(value).is_err());
}
#[test]
fn canonical_urls_and_credentials_are_handled_without_leaking_values() {
    assert_eq!(
        normalized_url("HTTP://LOCALHOST:80/root").unwrap(),
        "http://localhost/root/"
    );
    let mut config = safety();
    let mut duplicate = config.sources[0].clone();
    duplicate.id = Uuid::new_v4();
    if let SourceBackend::Alpaca { base_url, .. } = &mut duplicate.backend {
        *base_url = "http://127.0.0.1:32301".into();
    }
    config.sources.push(duplicate);
    invalid(&config, "duplicate");
    for url in [
        "http://alice:secret@localhost/",
        "http://localhost/?secret=token",
        "file:///tmp/x",
        "http://localhost/#secret",
    ] {
        let mut config = safety();
        if let SourceBackend::Alpaca { base_url, .. } = &mut config.sources[0].backend {
            *base_url = url.into();
        }
        invalid(&config, "url");
        assert!(
            !serde_json::to_string(&config.validate())
                .unwrap()
                .contains("secret")
        );
    }
    let mut config = safety();
    if let SourceBackend::Alpaca {
        credential_reference,
        ..
    } = &mut config.sources[0].backend
    {
        *credential_reference = Some("local-safety-token".into());
    }
    assert!(
        !serde_json::to_string(&config.export())
            .unwrap()
            .contains("local-safety-token")
    );
}
#[test]
fn reject_dangling_sources_types_and_indirect_cycles() {
    let mut config = safety();
    config.sources.remove(0);
    invalid(&config, "reference");
    let mut config = safety();
    config.sources[0].backend = SourceBackend::Simulated {
        device_type: DeviceType::Camera,
    };
    invalid(&config, "type");
    let mut config = safety();
    let output = config.outputs[0].id;
    config.sources[0].backend = SourceBackend::Virtual { output };
    invalid(&config, "cycle");
    let mut config = safety();
    let mut second = config.outputs[0].clone();
    second.id = Uuid::new_v4();
    second.number = 1;
    let first_id = config.outputs[0].id;
    config.sources[0].backend = SourceBackend::Virtual { output: second.id };
    config.sources[1].backend = SourceBackend::Virtual { output: first_id };
    config.outputs.push(second);
    invalid(&config, "cycle");
}
#[test]
fn invalid_range_or_timing_has_a_field_address() {
    let mut config = switches();
    if let VirtualDevice::Switch { channels } = &mut config.outputs[0].device {
        channels[0].step = 0.0;
    }
    assert!(
        config
            .validate()
            .iter()
            .any(|e| e.code == "range" && e.path.contains("channels[0]"))
    );
    let mut config = safety();
    if let VirtualDevice::Safety { members } = &mut config.outputs[0].device {
        members[0].policy.maximum_safe_age_seconds = 31.0;
    }
    assert!(
        config
            .validate()
            .iter()
            .any(|e| e.code == "timing" && e.path.ends_with("maximumSafeAgeSeconds"))
    );
}
#[test]
fn only_one_concurrent_editor_can_apply_a_revision() {
    let store = Arc::new(ConfigStore::new(None, safety()).unwrap());
    let candidate = store.snapshot();
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let (store, barrier, candidate) = (store.clone(), barrier.clone(), candidate.clone());
            std::thread::spawn(move || {
                barrier.wait();
                store.apply(candidate.revision, candidate, false)
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Err(ApplyError::Conflict)))
            .count(),
        1
    );
}

#[test]
fn prepared_updates_are_unpublished_store_bound_and_revision_checked_again_at_commit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.json");
    let store = ConfigStore::new(Some(path.clone()), safety()).unwrap();
    let before = store.snapshot();
    std::fs::write(&path, serde_json::to_vec(&before).unwrap()).unwrap();
    let first = store
        .prepare(before.revision, before.clone(), false)
        .unwrap();
    let stale = store
        .prepare(before.revision, before.clone(), false)
        .unwrap();
    assert_eq!(ConfigStore::load(&path).unwrap().snapshot(), before);
    assert_eq!(store.snapshot(), before);
    let other = ConfigStore::new(None, before.clone()).unwrap();
    let foreign = other
        .prepare(before.revision, before.clone(), false)
        .unwrap();
    assert!(matches!(store.commit(foreign), Err(ApplyError::Conflict)));
    let after = store.commit(first).unwrap();
    assert!(matches!(store.commit(stale), Err(ApplyError::Conflict)));
    assert_eq!(ConfigStore::load(&path).unwrap().snapshot(), after);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[cfg(unix)]
#[test]
fn a_failed_directory_flush_reports_the_committed_revision_instead_of_rollback() {
    use std::os::unix::fs::PermissionsExt;
    if unsafe { libc::geteuid() } == 0 {
        return;
    } // Root bypasses this permission fault.
    let dir = tempfile::tempdir().unwrap();
    struct Restore(std::path::PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    let _restore = Restore(dir.path().to_path_buf());
    let path = dir.path().join("hub.json");
    let store = ConfigStore::new(Some(path.clone()), safety()).unwrap();
    let before = store.snapshot();
    std::fs::write(&path, serde_json::to_vec(&before).unwrap()).unwrap();
    let staged = store
        .prepare(before.revision, before.clone(), false)
        .unwrap();
    // Rename remains permitted, but opening the directory to flush is denied.
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o300)).unwrap();
    let result = store.commit(staged);
    match result {
        Err(ApplyError::Committed {
            configuration,
            error,
        }) => {
            assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
            assert_ne!(configuration.revision, before.revision);
            assert_eq!(*configuration, store.snapshot());
            assert_eq!(*configuration, ConfigStore::load(&path).unwrap().snapshot());
        }
        other => panic!("Expected committed durability warning: {other:?}"),
    }
}
#[test]
fn failed_validation_connection_or_disk_write_preserves_live_config() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(Some(dir.path().join("missing/config.json")), safety()).unwrap();
    let before = store.snapshot();
    let mut candidate = before.clone();
    candidate.sources[0].id = Uuid::nil();
    assert!(matches!(
        store.apply(before.revision, candidate, false),
        Err(ApplyError::Invalid(_))
    ));
    assert!(matches!(
        store.apply(before.revision, before.clone(), true),
        Err(ApplyError::Connected)
    ));
    assert!(matches!(
        store.apply(before.revision, before.clone(), false),
        Err(ApplyError::Io(_))
    ));
    assert_eq!(store.snapshot(), before);
}
#[test]
fn deletion_restart_and_readdition_cannot_reassign_numbers_or_ids() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.json");
    let store = ConfigStore::new(Some(path.clone()), switches()).unwrap();
    let mut candidate = store.snapshot();
    let removed = candidate.outputs.remove(0);
    store.apply(candidate.revision, candidate, false).unwrap();
    let reloaded = ConfigStore::load(&path).unwrap();
    let mut candidate = reloaded.snapshot();
    let mut replacement = removed.clone();
    replacement.id = Uuid::new_v4();
    candidate.outputs.push(replacement);
    assert!(matches!(
        reloaded.apply(candidate.revision, candidate, false),
        Err(ApplyError::Invalid(_))
    ));
    let mut candidate = reloaded.snapshot();
    candidate.outputs.push(removed);
    let restored = reloaded
        .apply(candidate.revision, candidate, false)
        .unwrap();
    assert_eq!(ConfigStore::load(&path).unwrap().snapshot(), restored);
}
#[test]
fn channel_retarget_and_history_edits_are_rejected_after_removal() {
    let store = ConfigStore::new(None, switches()).unwrap();
    let mut candidate = store.snapshot();
    if let VirtualDevice::Switch { channels } = &mut candidate.outputs[0].device {
        channels[0].readout = channels[1].readout.clone();
    }
    assert!(matches!(
        store.apply(candidate.revision, candidate, false),
        Err(ApplyError::Invalid(_))
    ));
    let mut candidate = store.snapshot();
    candidate.identities = IdentityLedger::default();
    assert!(matches!(
        store.apply(candidate.revision, candidate, false),
        Err(ApplyError::Invalid(_))
    ));
    let mut candidate = store.snapshot();
    let removed = if let VirtualDevice::Switch { channels } = &mut candidate.outputs[0].device {
        channels.remove(1)
    } else {
        unreachable!()
    };
    let mut after = store.apply(candidate.revision, candidate, false).unwrap();
    if let VirtualDevice::Switch { channels } = &mut after.outputs[0].device {
        let mut replacement = removed;
        replacement.id = Uuid::new_v4();
        channels.push(replacement);
    }
    assert!(matches!(
        store.apply(after.revision, after, false),
        Err(ApplyError::Invalid(_))
    ));
}
#[test]
fn source_identity_cannot_be_retargeted_and_direct_sdk_share_claim() {
    let store = ConfigStore::new(None, safety()).unwrap();
    let mut candidate = store.snapshot();
    if let SourceBackend::Alpaca { base_url, .. } = &mut candidate.sources[0].backend {
        *base_url = "http://different.example/".into();
    }
    assert!(matches!(
        store.apply(candidate.revision, candidate, false),
        Err(ApplyError::Invalid(_))
    ));
    let mut config = HubConfig::empty();
    for device in [NativeDevice::CameraDirect, NativeDevice::CameraSdk] {
        config.sources.push(SourceConfig {
            id: Uuid::new_v4(),
            label: "Camera".into(),
            backend: SourceBackend::Native {
                device,
                identity: "ONE-CAMERA".into(),
            },
            polling: Default::default(),
        });
    }
    invalid(&config, "duplicate");
}
