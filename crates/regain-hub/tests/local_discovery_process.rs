//! A self-hosted private catalog worker. No installed registrations, activation,
//! equipment, network or production worker files are used by this fixture.
use regain_hub::{
    config::{Bitness, ConfigStore, DeviceType, HubConfig},
    discovery::{BlockedReason, Target, discover},
    factory::NoCredentials,
    native::NativeRuntime,
    runtime::HubRuntime,
    safety::MonotonicClock,
    service::{HubService, UpdateError},
    source::ErrorKind,
};
use serde_json::json;
use std::{io::BufRead, sync::Arc, time::Duration};
use uuid::Uuid;

fn fixture(args: &[String]) {
    assert_eq!(args.len(), 5);
    assert_eq!(args[0], "--discover");
    assert_eq!(args[1], "--device-type");
    assert_eq!(args[3], "--bitness");
    assert_eq!(args[4], "x64");
    let line = std::io::stdin().lock().lines().next().unwrap().unwrap();
    assert_eq!(line, r#"{"command":"discover"}"#);
    let directory = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    if args[2] == "focuser" {
        std::fs::write(directory.join("entered"), b"owned").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !directory.join("release").exists() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let mut entries = vec![
        json!({"name":"[FIXTURE] registered driver","progId":"Fixture.Valid","classId":"11111111-1111-4111-8111-111111111111"}),
        json!({"name":"[FIXTURE] missing class","progId":"Fixture.Missing","classId":null}),
        json!({"name":"[FIXTURE] alias of own output","progId":"Fixture.Alias","classId":"22222222-2222-4222-8222-222222222222"}),
    ];
    if args[2] == "camera" {
        entries.push(entries[0].clone());
    }
    if args[2] == "rotator" {
        entries[0]["unexpected"] = json!(true);
    }
    if args[2] == "safetymonitor" {
        eprintln!("private worker diagnostic must not escape");
    }
    if args[2] == "observingconditions" {
        std::process::exit(2);
    }
    println!("{}", json!({"entries":entries,"incomplete":false}));
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "--discover") {
        fixture(&args);
        return;
    }
    if !cfg!(windows) {
        println!("COM catalogs are Windows-only; native catalogs have separate portable tests");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let worker_directory = directory.path().join("hub-ascom/x64");
    std::fs::create_dir_all(&worker_directory).unwrap();
    std::fs::copy(
        std::env::current_exe().unwrap(),
        worker_directory.join("Regain.Hub.ASCOM.exe"),
    )
    .unwrap();
    let native = NativeRuntime {
        directory: directory.path().into(),
        simulate: true,
        references: None,
        cameras: None,
    };
    let revision = Uuid::new_v4();
    let self_class = Uuid::parse_str("22222222-2222-4222-8222-222222222222").unwrap();
    for device_type in [DeviceType::Switch, DeviceType::SafetyMonitor] {
        let catalog = discover(
            native.clone(),
            Target::Com {
                device_type,
                bitness: Bitness::X64,
            },
            revision,
            vec![self_class],
        )
        .await
        .unwrap();
        assert!(!catalog.simulated);
        assert_eq!(catalog.incomplete, device_type == DeviceType::SafetyMonitor);
        assert_eq!(catalog.entries.len(), 3);
        assert_eq!(
            catalog
                .entries
                .iter()
                .filter(|e| e.blocked_reason == Some(BlockedReason::MissingRegistration))
                .count(),
            1
        );
        assert_eq!(
            catalog
                .entries
                .iter()
                .filter(|e| e.blocked_reason == Some(BlockedReason::SelfProxy))
                .count(),
            1
        );
    }
    for device_type in [
        DeviceType::Camera,
        DeviceType::Rotator,
        DeviceType::ObservingConditions,
    ] {
        let error = discover(
            native.clone(),
            Target::Com {
                device_type,
                bitness: Bitness::X64,
            },
            revision,
            vec![],
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Unavailable);
        assert!(!error.message.contains("private worker diagnostic"));
    }
    assert_eq!(
        discover(
            native.clone(),
            Target::Com {
                device_type: DeviceType::Switch,
                bitness: Bitness::X86
            },
            revision,
            vec![]
        )
        .await
        .unwrap_err()
        .kind,
        ErrorKind::Unsupported
    );
    println!(
        "COM parent: fixed architecture/class arguments, ownership barrier, aliases, missing registrations, diagnostics, strict JSON and duplicate rejection passed"
    );

    let config = HubConfig::empty();
    let builder_native = native.clone();
    let service = HubService::persistent(
        ConfigStore::new(None, config.clone()).unwrap(),
        Arc::new(move |config| {
            HubRuntime::build(
                config,
                &builder_native,
                &NoCredentials,
                Arc::new(MonotonicClock::default()),
            )
        }),
    )
    .unwrap();
    let probe = tokio::spawn({
        let service = service.clone();
        let revision = config.revision;
        async move {
            service
                .discover_local(
                    Target::Com {
                        device_type: DeviceType::Focuser,
                        bitness: Bitness::X64,
                    },
                    revision,
                )
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        while !worker_directory.join("entered").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    probe.abort();
    assert!(probe.await.unwrap_err().is_cancelled());
    assert_eq!(service.runtime().unwrap().active_connections(), 1);
    assert!(matches!(
        service.apply(config.revision, config.clone()).await,
        Err(UpdateError::Connected)
    ));
    let shutdown = tokio::spawn({
        let service = service.clone();
        async move { service.shutdown().await }
    });
    tokio::task::yield_now().await;
    assert!(!shutdown.is_finished());
    std::fs::write(worker_directory.join("release"), b"release finite probe").unwrap();
    tokio::time::timeout(Duration::from_secs(3), shutdown)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(service.configuration(), config);
    println!(
        "Retained discovery: RPC loss keeps one owner, blocks apply, and shutdown drains the finite worker without retry or configuration changes"
    );
}
