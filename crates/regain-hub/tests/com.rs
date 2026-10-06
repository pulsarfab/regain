//! Real registered COM fixtures; script-owned private registration, never hardware.
#![cfg(windows)]
use regain_hub::{
    com::{ComBackend, available_architectures},
    config::{
        Bitness, ConnectionPolicy, DeviceType, HubConfig, Measurement, NativeDevice, OutputConfig,
        Readout, SafetyMember, SourceBackend, SourceConfig, SwitchChannel, VirtualDevice,
        WeatherMetric,
    },
    factory::NoCredentials,
    filterwheel::FilterWheelProperty,
    native::NativeRuntime,
    parameters::{PollPolicy, SafetyPolicy},
    rotator::RotatorProperty,
    runtime::HubRuntime,
    safety::MonotonicClock,
    sampling::SampleRequest,
    service::HubService,
    source::{Backend, ConnectionMethod, ErrorKind, SourceHandle, Values},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};
use uuid::Uuid;

struct Fixture {
    directory: PathBuf,
    prefix: String,
    native: NativeRuntime,
}
impl Fixture {
    fn load() -> Option<Self> {
        let Some(directory) = std::env::var_os("REGAIN_HUB_COM_FIXTURE_DIRECTORY") else {
            eprintln!("Run scripts/test-hub-com.ps1 for registered COM/parent integration tests");
            return None;
        };
        let native = NativeRuntime {
            directory: PathBuf::from(std::env::var_os("REGAIN_TEST_WORKERS").unwrap()),
            simulate: true,
            references: None,
        };
        assert_eq!(available_architectures(&native).len(), 2);
        Some(Self {
            directory: PathBuf::from(directory),
            prefix: std::env::var("REGAIN_HUB_COM_FIXTURE_PROGID").unwrap(),
            native,
        })
    }
    fn path(&self, name: &str) -> PathBuf {
        self.directory.join(format!("{}.{name}.json", self.prefix))
    }
    fn state(&self, name: &str, value: Value) {
        let file = self.path(name);
        let next = file.with_extension("new");
        std::fs::write(&next, serde_json::to_vec(&value).unwrap()).unwrap();
        std::fs::rename(next, file).unwrap();
    }
    fn clear(&self, name: &str, value: Value) {
        let _ = std::fs::remove_file(self.trace_path(name));
        self.state(name, value);
    }
    fn trace_path(&self, name: &str) -> PathBuf {
        PathBuf::from(format!("{}.trace", self.path(name).display()))
    }
    fn trace(&self, name: &str) -> Vec<Value> {
        std::fs::read_to_string(self.trace_path(name))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }
    fn count(&self, name: &str, member: &str) -> usize {
        self.trace(name)
            .iter()
            .filter(|t| t["member"] == member)
            .count()
    }
    fn pid(&self, name: &str) -> u32 {
        self.trace(name)
            .iter()
            .rev()
            .find(|t| t["member"] == "Activate")
            .unwrap()["pid"]
            .as_u64()
            .unwrap() as u32
    }
    fn source(&self, name: &str, device: DeviceType, bitness: Bitness) -> SourceConfig {
        SourceConfig {
            id: Uuid::new_v4(),
            label: format!("Registered COM fixture {name}"),
            backend: SourceBackend::Com {
                prog_id: format!("{}.{name}", self.prefix),
                device_type: device,
                bitness,
                connection_policy: ConnectionPolicy::Managed,
            },
            polling: PollPolicy {
                poll_seconds: 0.1,
                request_timeout_seconds: 3.0,
                connection_timeout_seconds: 10.0,
                initial_backoff_seconds: 0.05,
                backoff_cap_seconds: 0.1,
                ..PollPolicy::default()
            },
        }
    }
    fn backend(&self, source: &SourceConfig, samples: Vec<SampleRequest>) -> ComBackend {
        ComBackend::new(source, &self.native, samples).unwrap()
    }
}
async fn until(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
fn alive(pid: u32) -> bool {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    };
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return false;
        }
        let mut code = 0;
        let running = GetExitCodeProcess(process, &mut code) != 0 && code == 259;
        CloseHandle(process);
        running
    }
}
fn gauge(number: u32, readout: Readout, writable: bool) -> SwitchChannel {
    SwitchChannel {
        id: Uuid::new_v4(),
        number,
        label: format!("Gauge {number}"),
        readout,
        writable,
        minimum: -40.0,
        maximum: 100.0,
        step: 0.5,
        units: "".into(),
    }
}
fn switches(channels: Vec<SwitchChannel>) -> OutputConfig {
    OutputConfig {
        id: Uuid::new_v4(),
        number: 0,
        label: "COM mixed controls".into(),
        device: VirtualDevice::Switch { channels },
    }
}

#[test]
fn preparation_checks_architecture_and_class_without_activation() {
    let Some(f) = Fixture::load() else {
        return;
    };
    f.clear("Switch", json!({}));
    let source = f.source("Switch", DeviceType::Switch, Bitness::X86);
    let _ = f.backend(&source, Vec::new());
    assert_eq!(f.count("Switch", "Activate"), 0);
    let mut camera = source.clone();
    if let SourceBackend::Com { device_type, .. } = &mut camera.backend {
        *device_type = DeviceType::Camera;
    }
    assert!(
        matches!(ComBackend::new(&camera, &f.native, Vec::new()), Err(e) if e.kind == ErrorKind::Unsupported)
    );
    let missing = NativeRuntime {
        directory: PathBuf::from("nonexistent-com-helper-directory"),
        simulate: false,
        references: None,
    };
    assert!(available_architectures(&missing).is_empty());
    assert!(
        matches!(ComBackend::new(&source, &missing, Vec::new()), Err(e) if e.kind == ErrorKind::Unsupported)
    );
}

fn wheel_config(source: SourceConfig) -> HubConfig {
    let mut config = HubConfig::empty();
    for number in [2, 17] {
        config.outputs.push(OutputConfig {
            id: Uuid::new_v4(),
            number,
            label: format!("Private COM wheel {number}"),
            device: VirtualDevice::Proxy {
                source: source.id,
                device_type: DeviceType::FilterWheel,
            },
        });
    }
    config.sources.push(source);
    config
}

#[tokio::test]
async fn typed_wheel_com_factory_preserves_metadata_shared_leases_and_unknown_setter_fences() {
    let Some(f) = Fixture::load() else {
        return;
    };
    for (bitness, version) in [(Bitness::X86, 2), (Bitness::X64, 3)] {
        f.clear("Wheel", json!({"version":version}));
        let source = f.source("Wheel", DeviceType::FilterWheel, bitness);
        let id = source.id;
        let config = wheel_config(source);
        let hub = HubRuntime::build(
            config.clone(),
            &f.native,
            &NoCredentials,
            Arc::new(MonotonicClock::default()),
        )
        .unwrap();
        assert_eq!(f.count("Wheel", "Activate"), 0);
        let first = hub.client();
        let second = hub.client();
        first.connect(config.outputs[0].id).await.unwrap();
        second.connect(config.outputs[1].id).await.unwrap();
        let a = first.connection(config.outputs[0].id).unwrap();
        let b = second.connection(config.outputs[1].id).unwrap();
        let aw = a.filterwheel().unwrap();
        let bw = b.filterwheel().unwrap();
        assert_eq!(aw.generation(), bw.generation());
        assert_eq!(
            aw.property(FilterWheelProperty::Names).await.unwrap(),
            json!(["L", "Hα", ""])
        );
        assert_eq!(
            bw.property(FilterWheelProperty::FocusOffsets)
                .await
                .unwrap(),
            json!([-12, 0, 17])
        );
        until(|| {
            let state = hub.source_snapshot(id).unwrap();
            state.values.get("names") == Some(&json!(["L", "Hα", ""]))
                && state.values.get("focusoffsets") == Some(&json!([-12, 0, 17]))
        })
        .await;
        assert_eq!(f.count("Wheel", "Activate"), 1);
        assert_eq!(f.count("Wheel", "Connect"), usize::from(version == 3));
        assert_eq!(f.count("Wheel", "Connected.set"), usize::from(version == 2));
        assert_eq!(
            aw.move_to(3).await.unwrap_err().kind,
            ErrorKind::InvalidValue
        );
        assert_eq!(f.count("Wheel", "Position.set"), 0);
        aw.move_to(2).await.unwrap();
        assert_eq!(bw.position().await.unwrap(), 2);
        f.state(
            "Wheel",
            json!({"version":version,"wheelOffsetsCase":"boundaries"}),
        );
        assert_eq!(
            bw.property(FilterWheelProperty::FocusOffsets)
                .await
                .unwrap(),
            json!([i32::MIN, 0, i32::MAX])
        );
        f.state("Wheel", json!({"version":version,"badWheelPosition":true}));
        assert_eq!(
            bw.move_to(0).await.unwrap_err().kind,
            ErrorKind::Unavailable
        );
        assert_eq!(f.count("Wheel", "Position.set"), 1);
        f.state("Wheel", json!({"version":version}));
        first.disconnect(config.outputs[0].id);
        drop(a);
        until(|| hub.source_snapshot(id).unwrap().lease_count == 1).await;
        assert!(bw.connected());
        assert_eq!(f.count("Wheel", "Disconnect"), 0);
        f.state(
            "Wheel",
            json!({"version":version,"argumentFaultMember":"Position.set"}),
        );
        assert_eq!(bw.move_to(0).await.unwrap_err().kind, ErrorKind::Uncertain);
        f.state("Wheel", json!({"version":version}));
        assert_eq!(bw.move_to(1).await.unwrap_err().kind, ErrorKind::Uncertain);
        assert_eq!(f.count("Wheel", "Position.set"), 2);
        assert_eq!(f.count("Wheel", "Halt"), 0);
        assert!(hub.source_snapshot(id).unwrap().write_uncertain);
        second.disconnect(config.outputs[1].id);
        drop(b);
        hub.shutdown().await.unwrap();
        assert_eq!(f.count("Wheel", "SetupDialog"), 0);
    }
}

#[tokio::test]
async fn typed_wheel_com_rejects_mismatched_metadata_before_accepting_motion() {
    let Some(f) = Fixture::load() else {
        return;
    };
    for bitness in [Bitness::X86, Bitness::X64] {
        f.clear("Wheel", json!({"version":3,"wheelOffsetsCase":"mismatch"}));
        let config = wheel_config(f.source("Wheel", DeviceType::FilterWheel, bitness));
        let hub = HubRuntime::build(
            config.clone(),
            &f.native,
            &NoCredentials,
            Arc::new(MonotonicClock::default()),
        )
        .unwrap();
        let client = hub.client();
        assert_eq!(
            client.connect(config.outputs[0].id).await.unwrap_err().kind,
            ErrorKind::Unavailable
        );
        assert_eq!(f.count("Wheel", "Position.set"), 0);
        hub.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn typed_rotator_com_factory_shares_leases_and_preserves_source_coordinates_and_failures() {
    let Some(f) = Fixture::load() else {
        return;
    };
    for (bitness, version) in [(Bitness::X86, 3), (Bitness::X64, 4)] {
        f.clear("Rotator", json!({"version":version}));
        let source = f.source("Rotator", DeviceType::Rotator, bitness);
        let source_id = source.id;
        let mut config = HubConfig::empty();
        config.sources.push(source);
        for number in [2, 17] {
            config.outputs.push(OutputConfig {
                id: Uuid::new_v4(),
                number,
                label: format!("Private COM rotator {number}"),
                device: VirtualDevice::Proxy {
                    source: source_id,
                    device_type: DeviceType::Rotator,
                },
            });
        }
        let hub = HubRuntime::build(
            config.clone(),
            &f.native,
            &NoCredentials,
            Arc::new(MonotonicClock::default()),
        )
        .unwrap();
        assert_eq!(f.count("Rotator", "Activate"), 0);
        let first = hub.client();
        let second = hub.client();
        first.connect(config.outputs[0].id).await.unwrap();
        second.connect(config.outputs[1].id).await.unwrap();
        let a = first.connection(config.outputs[0].id).unwrap();
        let b = second.connection(config.outputs[1].id).unwrap();
        let ar = a.rotator().unwrap();
        let br = b.rotator().unwrap();
        assert_eq!(ar.generation(), br.generation());
        for (property, expected) in [
            (RotatorProperty::Position, json!(20.0)),
            (RotatorProperty::MechanicalPosition, json!(350.0)),
            (RotatorProperty::TargetPosition, json!(20.0)),
            (RotatorProperty::IsMoving, json!(false)),
            (RotatorProperty::Reverse, json!(false)),
            (RotatorProperty::CanReverse, json!(true)),
            (RotatorProperty::StepSize, json!(0.02)),
        ] {
            let actual = ar.property(property).await.unwrap();
            if expected.is_boolean() {
                assert_eq!(actual.as_bool(), expected.as_bool());
            } else {
                assert_eq!(actual.as_f64(), expected.as_f64());
            }
        }
        assert_eq!(f.count("Rotator", "Activate"), 1);
        assert_eq!(f.count("Rotator", "Connect"), usize::from(version == 4));
        assert_eq!(
            f.count("Rotator", "Connected.set"),
            usize::from(version == 3)
        );
        assert_eq!(
            ar.move_absolute(360.0).await.unwrap_err().kind,
            ErrorKind::InvalidValue
        );
        assert_eq!(f.count("Rotator", "MoveAbsolute"), 0);
        ar.move_relative(-721.5).await.unwrap();
        assert_eq!(
            br.property(RotatorProperty::TargetPosition)
                .await
                .unwrap()
                .as_f64(),
            Some(18.5)
        );
        assert_eq!(
            br.move_absolute(40.0).await.unwrap_err().kind,
            ErrorKind::Busy
        );
        br.halt().await.unwrap();
        br.sync(15.0).await.unwrap();
        assert_eq!(
            ar.property(RotatorProperty::Position)
                .await
                .unwrap()
                .as_f64(),
            Some(15.0)
        );
        assert_eq!(
            ar.property(RotatorProperty::MechanicalPosition)
                .await
                .unwrap()
                .as_f64(),
            Some(350.0)
        );
        ar.move_absolute(42.5).await.unwrap();
        assert_eq!(
            br.property(RotatorProperty::TargetPosition)
                .await
                .unwrap()
                .as_f64(),
            Some(42.5)
        );
        br.halt().await.unwrap();
        br.move_mechanical(355.0).await.unwrap();
        assert_eq!(
            ar.property(RotatorProperty::MechanicalPosition)
                .await
                .unwrap()
                .as_f64(),
            Some(355.0)
        );
        assert_eq!(
            ar.property(RotatorProperty::TargetPosition)
                .await
                .unwrap()
                .as_f64(),
            Some(20.0)
        );
        ar.halt().await.unwrap();
        ar.set_reverse(true).await.unwrap();
        assert_eq!(
            br.property(RotatorProperty::Reverse).await.unwrap(),
            json!(true)
        );
        f.state(
            "Rotator",
            json!({"version":version, "faultMember":"StepSize", "faultCode":-2147220480i32}),
        );
        assert_eq!(
            ar.property(RotatorProperty::StepSize)
                .await
                .unwrap_err()
                .kind,
            ErrorKind::Unsupported
        );
        assert_eq!(
            ar.property(RotatorProperty::Position)
                .await
                .unwrap()
                .as_f64(),
            Some(15.0)
        );
        f.state("Rotator", json!({"version":version,"badMoving":true}));
        let moves = f.count("Rotator", "Move");
        assert_eq!(
            ar.move_relative(1.0).await.unwrap_err().kind,
            ErrorKind::Unavailable
        );
        assert_eq!(f.count("Rotator", "Move"), moves);
        f.state("Rotator", json!({"version":version}));
        first.disconnect(config.outputs[0].id);
        drop(a);
        until(|| hub.source_snapshot(source_id).unwrap().lease_count == 1).await;
        assert_eq!(f.count("Rotator", "Disconnect"), 0);
        assert!(br.connected());
        f.state(
            "Rotator",
            json!({"version":version,"faultMember":"Move","faultCode":-2147220225i32}),
        );
        assert_eq!(
            br.move_relative(5.0).await.unwrap_err().kind,
            ErrorKind::Uncertain
        );
        f.state("Rotator", json!({"version":version}));
        let halts = f.count("Rotator", "Halt");
        assert_eq!(br.halt().await.unwrap_err().kind, ErrorKind::Uncertain);
        assert_eq!(
            br.set_reverse(false).await.unwrap_err().kind,
            ErrorKind::Uncertain
        );
        assert_eq!(f.count("Rotator", "Move"), moves + 1);
        assert_eq!(f.count("Rotator", "Halt"), halts);
        assert!(hub.source_snapshot(source_id).unwrap().write_uncertain);
        second.disconnect(config.outputs[1].id);
        drop(b);
        hub.shutdown().await.unwrap();
        assert_eq!(f.count("Rotator", "SetupDialog"), 0);
    }
}

#[tokio::test]
async fn modern_com_rotator_cannot_connect_without_required_reversal() {
    let Some(f) = Fixture::load() else {
        return;
    };
    for bitness in [Bitness::X86, Bitness::X64] {
        for version in [3, 4] {
            f.clear("Rotator", json!({"version":version,"noReverse":true}));
            let source = f.source("Rotator", DeviceType::Rotator, bitness);
            let source_id = source.id;
            let mut config = HubConfig::empty();
            config.sources.push(source);
            config.outputs.push(OutputConfig {
                id: Uuid::new_v4(),
                number: 5,
                label: "Invalid modern rotator".into(),
                device: VirtualDevice::Proxy {
                    source: source_id,
                    device_type: DeviceType::Rotator,
                },
            });
            let output = config.outputs[0].id;
            let hub = HubRuntime::build(
                config,
                &f.native,
                &NoCredentials,
                Arc::new(MonotonicClock::default()),
            )
            .unwrap();
            let client = hub.client();
            assert_eq!(
                client.connect(output).await.unwrap_err().kind,
                ErrorKind::Unavailable
            );
            assert!(client.connection(output).is_err());
            assert_eq!(f.count("Rotator", "Move"), 0);
            client.close();
            hub.shutdown().await.unwrap();
        }
    }
}

#[tokio::test]
async fn typed_focuser_com_factory_keeps_int32_limits_shared_leases_and_generation_fences() {
    let Some(f) = Fixture::load() else {
        return;
    };
    for (bitness, version) in [(Bitness::X86, 3), (Bitness::X64, 4)] {
        f.clear("Focuser", json!({"version":version}));
        let source = f.source("Focuser", DeviceType::Focuser, bitness);
        let source_id = source.id;
        let mut config = HubConfig::empty();
        config.sources.push(source);
        for number in [4, 7] {
            config.outputs.push(OutputConfig {
                id: Uuid::new_v4(),
                number,
                label: format!("Private COM focuser {number}"),
                device: VirtualDevice::Proxy {
                    source: source_id,
                    device_type: DeviceType::Focuser,
                },
            });
        }
        let hub = HubRuntime::build(
            config.clone(),
            &f.native,
            &NoCredentials,
            Arc::new(MonotonicClock::default()),
        )
        .unwrap();
        assert_eq!(f.count("Focuser", "Activate"), 0);
        let first = hub.client();
        let second = hub.client();
        first.connect(config.outputs[0].id).await.unwrap();
        second.connect(config.outputs[1].id).await.unwrap();
        let a = first.connection(config.outputs[0].id).unwrap();
        let b = second.connection(config.outputs[1].id).unwrap();
        assert_eq!(
            a.focuser().unwrap().generation(),
            b.focuser().unwrap().generation()
        );
        assert_eq!(
            a.focuser().unwrap().capabilities().await.unwrap().max_step,
            100000
        );
        assert_eq!(a.focuser().unwrap().temperature().await.unwrap(), 12.5);
        assert_eq!(a.focuser().unwrap().step_size().await.unwrap(), 1.25);
        assert_eq!(f.count("Focuser", "Activate"), 1);
        assert_eq!(f.count("Focuser", "Connect"), usize::from(version == 4));
        assert_eq!(
            f.count("Focuser", "Connected.set"),
            usize::from(version == 3)
        );
        assert_eq!(
            a.focuser().unwrap().move_to(70000).await.unwrap_err().kind,
            ErrorKind::InvalidValue
        );
        assert_eq!(f.count("Focuser", "Move"), 0);
        a.focuser().unwrap().move_to(70).await.unwrap();
        assert!(b.focuser().unwrap().is_moving().await.unwrap());
        assert_eq!(
            b.focuser().unwrap().move_to(80).await.unwrap_err().kind,
            ErrorKind::Busy
        );
        b.focuser().unwrap().halt().await.unwrap();
        b.focuser().unwrap().set_temp_comp(true).await.unwrap();
        assert!(a.focuser().unwrap().temp_comp().await.unwrap());
        first.disconnect(config.outputs[0].id);
        drop(a);
        until(|| hub.source_snapshot(source_id).unwrap().lease_count == 1).await;
        assert_eq!(b.focuser().unwrap().position().await.unwrap(), 70);
        assert_eq!(f.count("Focuser", "Disconnect"), 0);
        // A dispatched vendor failure remains uncertain for both output clients.
        f.state(
            "Focuser",
            json!({"version":version, "faultMember":"Move", "faultCode":-2147220225i32}),
        );
        assert_eq!(
            b.focuser().unwrap().move_to(80).await.unwrap_err().kind,
            ErrorKind::Uncertain
        );
        assert_eq!(
            b.focuser().unwrap().move_to(90).await.unwrap_err().kind,
            ErrorKind::Uncertain
        );
        assert_eq!(f.count("Focuser", "Move"), 2);
        assert!(hub.source_snapshot(source_id).unwrap().write_uncertain);
        second.disconnect(config.outputs[1].id);
        drop(b);
        hub.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn factory_passes_all_own_export_classes_and_rejects_a_registered_alias_before_activation() {
    let Some(f) = Fixture::load() else {
        return;
    };
    for architecture in [Bitness::X86, Bitness::X64] {
        f.clear("Own", json!({}));
        let source = f.source("Own", DeviceType::Switch, architecture);
        let source_id = source.id;
        let output = OutputConfig {
            id: std::env::var("REGAIN_HUB_COM_SELF_OUTPUT")
                .unwrap()
                .parse()
                .unwrap(),
            ..switches(vec![gauge(
                0,
                Readout::Channel {
                    source: source.id,
                    channel: 0,
                    unit: None,
                },
                false,
            )])
        };
        let output_id = output.id;
        let mut config = HubConfig::empty();
        config.instance_id = std::env::var("REGAIN_HUB_COM_SELF_INSTANCE")
            .unwrap()
            .parse()
            .unwrap();
        config.sources.push(source);
        config.outputs.push(output);
        // This ordinary fixture alias is not recognisable from its ProgID;
        // the worker must resolve its actual registered native-output CLSID.
        assert!(config.validate().is_empty());
        let hub = HubRuntime::build(
            config,
            &f.native,
            &NoCredentials,
            Arc::new(MonotonicClock::default()),
        )
        .unwrap();
        let client = hub.client();
        // A scalar output stays connected for diagnostics when its source is
        // unavailable; the actor must reject activation and provide no reading.
        client.connect(output_id).await.unwrap();
        until(|| {
            hub.source_snapshot(source_id)
                .unwrap()
                .error
                .is_some_and(|error| error.kind == ErrorKind::InvalidValue)
        })
        .await;
        assert_eq!(f.count("Own", "Activate"), 0);
        assert!(hub.source_snapshot(source_id).unwrap().values.is_empty());
        client.disconnect(output_id);
        client.connect(output_id).await.unwrap();
        until(|| {
            hub.source_snapshot(source_id)
                .unwrap()
                .error
                .is_some_and(|error| error.kind == ErrorKind::InvalidValue)
        })
        .await;
        assert_eq!(f.count("Own", "Activate"), 0);
        client.close();
        hub.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn actual_parent_negotiates_connections_and_incremental_weather_without_dropping_other_sensors()
 {
    let Some(f) = Fixture::load() else {
        return;
    };
    for architecture in [Bitness::X86, Bitness::X64] {
        for (version, borrowed) in [(1, true), (2, false)] {
            f.clear("Weather", json!({"version":version,"initialConnected":true,"faultMember":"Humidity","faultCode":-2147220478i32}));
            let source = f.source("Weather", DeviceType::ObservingConditions, architecture);
            let samples = ["humidity", "temperature"]
                .map(|property| {
                    SampleRequest::readout(
                        &Readout::Property {
                            source: source.id,
                            property: property.into(),
                            unit: None,
                        },
                        true,
                    )
                })
                .to_vec();
            let mut backend = f.backend(&source, samples);
            backend.connect().await.unwrap();
            let info = backend.connection_info().unwrap();
            assert_eq!(info.owns_connection, !borrowed);
            assert_eq!(
                info.method,
                if version == 1 {
                    ConnectionMethod::Legacy
                } else {
                    ConnectionMethod::Async
                }
            );
            assert!(backend.sample().await.unwrap().more); // humidity age
            let error = backend.sample().await.unwrap();
            assert_eq!(error.errors["humidity"].kind, ErrorKind::Unavailable);
            assert!(error.more);
            assert!(backend.sample().await.unwrap().more); // temperature age
            tokio::time::sleep(Duration::from_millis(30)).await;
            let reading = backend.sample().await.unwrap();
            assert_eq!(reading.values["temperature"], 12.5);
            assert!(reading.ages_seconds["temperature"] >= 3.53);
            assert!(!reading.more);
            backend.refresh().await.unwrap();
            let pid = f.pid("Weather");
            backend.disconnect().await.unwrap();
            until(|| !alive(pid)).await;
            assert_eq!(f.count("Weather", "Disconnect"), usize::from(!borrowed));
            assert_eq!(f.count("Weather", "Dispose"), 0);
        }
    }
}

#[tokio::test]
async fn shared_factory_outputs_reuse_com_source_and_preserve_last_lease_ownership() {
    let Some(f) = Fixture::load() else {
        return;
    };
    for architecture in [Bitness::X86, Bitness::X64] {
        f.clear("Switch", json!({}));
        let source = f.source("Switch", DeviceType::Switch, architecture);
        let id = source.id;
        let mut control = gauge(
            0,
            Readout::Channel {
                source: id,
                channel: 0,
                unit: None,
            },
            true,
        );
        control.minimum = 0.0;
        let output = switches(vec![control]);
        let output_id = output.id;
        let mut config = HubConfig::empty();
        config.sources.push(source);
        config.outputs.push(output);
        let hub = HubRuntime::build(
            config,
            &f.native,
            &NoCredentials,
            Arc::new(MonotonicClock::default()),
        )
        .unwrap();
        let service = HubService::read_only(hub.clone());
        let capabilities = service.configuration_capabilities();
        for capability in ["comSources", "comX86Sources", "comX64Sources"] {
            assert!(capabilities.contains(&capability));
        }
        assert!(!capabilities.contains(&"broaderComSources"));
        assert_eq!(f.count("Switch", "Activate"), 0);
        let a = hub.client();
        let b = hub.client();
        a.connect(output_id).await.unwrap();
        b.connect(output_id).await.unwrap();
        until(|| {
            hub.source_snapshot(id)
                .unwrap()
                .values
                .contains_key("channel/0")
        })
        .await;
        let generation = hub.source_snapshot(id).unwrap().generation;
        assert_eq!(hub.source_snapshot(id).unwrap().lease_count, 2);
        assert_eq!(f.count("Switch", "Activate"), 1);
        assert_eq!(f.count("Switch", "Connect"), 1);
        a.connection(output_id)
            .unwrap()
            .switch()
            .unwrap()
            .set_value(0, 7.5)
            .await
            .unwrap();
        until(|| {
            b.connection(output_id)
                .unwrap()
                .switch()
                .unwrap()
                .value(0)
                .ok()
                == Some(7.5)
        })
        .await;
        assert_eq!(hub.source_snapshot(id).unwrap().generation, generation);
        a.change_connection(output_id, false, false).await.unwrap();
        assert_eq!(f.count("Switch", "Disconnect"), 0);
        assert_eq!(hub.source_snapshot(id).unwrap().lease_count, 1);
        b.change_connection(output_id, false, false).await.unwrap();
        until(|| f.count("Switch", "Disconnect") == 1).await;
        hub.shutdown().await.unwrap();
        assert_eq!(f.count("Switch", "Disconnect"), 1);
    }
}

#[tokio::test]
async fn cancelled_read_retires_worker_and_parent_job_does_not_kill_a_shared_vendor_child() {
    let Some(f) = Fixture::load() else {
        return;
    };
    for architecture in [Bitness::X86, Bitness::X64] {
        f.clear("Other", json!({"spawnHelper":true}));
        let marker = PathBuf::from(format!("{}.stop", f.path("Other").display()));
        let _ = std::fs::remove_file(&marker);
        let cfg = f.source("Other", DeviceType::Switch, architecture);
        let mut backend = f.backend(&cfg, Vec::new());
        backend.connect().await.unwrap();
        let worker = f.pid("Other");
        let helper = f
            .trace("Other")
            .iter()
            .find(|t| t["member"] == "SharedHelper")
            .unwrap()["value"]
            .as_u64()
            .unwrap() as u32;
        assert!(alive(helper));
        f.state("Other", json!({"hangMember":"Name"}));
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                backend.read("name".into(), Values::new())
            )
            .await
            .is_err()
        );
        backend.reset();
        until(|| !alive(worker)).await;
        assert!(
            alive(helper),
            "A shared vendor child must outlive Regain worker retirement"
        );
        std::fs::write(marker, b"stop private fixture").unwrap();
        until(|| !alive(helper)).await;
        f.state("Other", json!({}));
        backend.connect().await.unwrap();
        assert_ne!(f.pid("Other"), worker);
        assert_eq!(
            backend
                .read("maxswitch".into(), Values::new())
                .await
                .unwrap(),
            2
        );
        backend.disconnect().await.unwrap();
    }
}

#[tokio::test]
async fn actor_lost_write_is_sent_once_and_new_generation_cannot_clear_uncertainty() {
    let Some(f) = Fixture::load() else {
        return;
    };
    for architecture in [Bitness::X86, Bitness::X64] {
        f.clear("Switch", json!({}));
        let mut cfg = f.source("Switch", DeviceType::Switch, architecture);
        cfg.polling.request_timeout_seconds = 1.0;
        let sample = SampleRequest::readout(
            &Readout::Channel {
                source: cfg.id,
                channel: 0,
                unit: None,
            },
            false,
        );
        let source = SourceHandle::spawn(
            cfg.id,
            Uuid::new_v4(),
            cfg.polling.clone(),
            Box::new(f.backend(&cfg, vec![sample])),
            Arc::new(MonotonicClock::default()),
        )
        .unwrap();
        let lease = Uuid::new_v4();
        source.acquire(lease).await.unwrap();
        until(|| source.snapshot().transport_connected).await;
        let generation = source.snapshot().generation;
        let pid = f.pid("Switch");
        source.control(lease, true).await.unwrap();
        f.state("Switch", json!({"hangMember":"SetSwitchValue"}));
        let parameters = Values::from([("Id".into(), json!(0)), ("Value".into(), json!(5))]);
        assert_eq!(
            source
                .write(lease, "setswitchvalue", parameters.clone())
                .await
                .unwrap_err()
                .kind,
            ErrorKind::Uncertain
        );
        assert!(source.snapshot().write_uncertain);
        until(|| !alive(pid)).await;
        f.state("Switch", json!({}));
        until(|| {
            source.snapshot().transport_connected && source.snapshot().generation != generation
        })
        .await;
        assert!(source.snapshot().write_uncertain);
        assert_eq!(
            source
                .write(lease, "setswitchvalue", parameters)
                .await
                .unwrap_err()
                .kind,
            ErrorKind::Uncertain
        );
        assert_eq!(f.count("Switch", "SetSwitchValue"), 1);
        source.release(lease).await.unwrap();
        source.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn uncertain_connection_cannot_replay_after_worker_reset() {
    let Some(f) = Fixture::load() else {
        return;
    };
    for architecture in [Bitness::X86, Bitness::X64] {
        f.clear("Switch", json!({"hangMember":"Connect"}));
        let mut cfg = f.source("Switch", DeviceType::Switch, architecture);
        cfg.polling.request_timeout_seconds = 1.0;
        let mut backend = f.backend(&cfg, Vec::new());
        assert_eq!(
            backend.connect().await.unwrap_err().kind,
            ErrorKind::Uncertain
        );
        backend.reset();
        f.state("Switch", json!({}));
        assert_eq!(
            backend.connect().await.unwrap_err().kind,
            ErrorKind::Uncertain
        );
        assert_eq!(
            backend.disconnect().await.unwrap_err().kind,
            ErrorKind::Uncertain
        );
        assert_eq!(f.count("Switch", "Connect"), 1);
        assert_eq!(f.count("Switch", "Activate"), 1);
    }
}

#[tokio::test]
async fn cached_safe_evidence_expires_while_com_is_stalled_and_unrelated_weather_stays_available() {
    let Some(f) = Fixture::load() else {
        return;
    };
    for architecture in [Bitness::X86, Bitness::X64] {
        f.clear("Safety", json!({}));
        f.clear("Weather", json!({"version":2}));
        let safety_source = f.source("Safety", DeviceType::SafetyMonitor, architecture);
        let safety_id = safety_source.id;
        let weather_source = f.source("Weather", DeviceType::ObservingConditions, architecture);
        let weather_id = weather_source.id;
        let safety = OutputConfig {
            id: Uuid::new_v4(),
            number: 0,
            label: "COM safety".into(),
            device: VirtualDevice::Safety {
                members: vec![SafetyMember {
                    source: safety_id,
                    enabled: true,
                    policy: SafetyPolicy {
                        confirmation_seconds: 0.1,
                        failed_cycles_to_unsafe: 1000,
                        safe_readings_to_safe: 1,
                        maximum_safe_age_seconds: 4.5,
                        return_to_safe_hold_seconds: 0.0,
                        ..SafetyPolicy::default()
                    },
                }],
            },
        };
        let weather = OutputConfig {
            id: Uuid::new_v4(),
            number: 0,
            label: "COM weather".into(),
            device: VirtualDevice::Weather {
                measurements: BTreeMap::from([(
                    WeatherMetric::Temperature,
                    Measurement {
                        sources: vec![Readout::Property {
                            source: weather_id,
                            property: "temperature".into(),
                            unit: None,
                        }],
                        maximum_age_seconds: 30.0,
                        average_seconds: 0.0,
                    },
                )]),
            },
        };
        let safety_output = safety.id;
        let weather_output = weather.id;
        let mut config = HubConfig::empty();
        config.sources = vec![safety_source, weather_source];
        config.outputs = vec![safety, weather];
        let hub = HubRuntime::build(
            config,
            &f.native,
            &NoCredentials,
            Arc::new(MonotonicClock::default()),
        )
        .unwrap();
        let client = hub.client();
        client.connect(safety_output).await.unwrap();
        client.connect(weather_output).await.unwrap();
        let safety = client.connection(safety_output).unwrap();
        let weather = client.connection(weather_output).unwrap();
        until(|| safety.safety().unwrap().snapshot().is_safe).await;
        until(|| {
            weather
                .weather()
                .unwrap()
                .read(WeatherMetric::Temperature)
                .is_ok()
        })
        .await;
        let generation = hub.source_snapshot(safety_id).unwrap().generation;
        let pid = f.pid("Safety");
        f.state(
            "Safety",
            json!({"faultMember":"IsSafe","faultCode":-2147220225i32}),
        );
        // Retain grace until evidence is already old; then start a long request
        // whose deadline lies AFTER safe age expiry. Getters do not poll.
        tokio::time::sleep(Duration::from_millis(2700)).await;
        assert!(safety.safety().unwrap().snapshot().is_safe);
        f.state("Safety", json!({"hangMember":"IsSafe"}));
        let polls = f.count("Safety", "IsSafe");
        until(|| f.count("Safety", "IsSafe") > polls).await;
        until(|| !safety.safety().unwrap().snapshot().is_safe).await;
        assert!(
            alive(pid),
            "Evidence must expire before the hung request/worker deadline"
        );
        assert_eq!(
            hub.source_snapshot(safety_id).unwrap().generation,
            generation
        );
        assert_eq!(
            weather
                .weather()
                .unwrap()
                .read(WeatherMetric::Temperature)
                .unwrap()
                .value,
            12.5
        );
        f.state("Safety", json!({}));
        until(|| {
            hub.source_snapshot(safety_id).unwrap().generation != generation
                && safety.safety().unwrap().snapshot().is_safe
        })
        .await;
        drop(safety);
        drop(weather);
        client.close();
        hub.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn mixed_native_alpaca_and_com_gauges_share_the_same_controller() {
    let Some(f) = Fixture::load() else {
        return;
    };
    let server = axum::Router::new().fallback(axum::routing::any(
        |request: axum::extract::Request| async move {
            let member = request.uri().path().rsplit('/').next().unwrap();
            let value = match member {
                "interfaceversion" => json!(2),
                "connected" => json!(true),
                "getswitchvalue" => json!(42),
                _ => Value::Null,
            };
            axum::Json(json!({"ErrorNumber":0,"Value":value}))
        },
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, server).await.unwrap();
    });
    for architecture in [Bitness::X86, Bitness::X64] {
        f.clear("Switch", json!({}));
        let com = f.source("Switch", DeviceType::Switch, architecture);
        let native = SourceConfig {
            id: Uuid::new_v4(),
            label: "Explicit native FC3 simulation".into(),
            backend: SourceBackend::Native {
                device: NativeDevice::Fc3,
                identity: "00:00:00:00:00:03".into(),
                filter_wheel: None,
            },
            polling: com.polling.clone(),
        };
        let remote = SourceConfig {
            id: Uuid::new_v4(),
            label: "Loopback Alpaca fixture".into(),
            backend: SourceBackend::Alpaca {
                base_url: format!("http://{address}"),
                device_type: DeviceType::Switch,
                device_number: 0,
                connection_policy: ConnectionPolicy::ExternallyManaged,
                credential_reference: None,
            },
            polling: com.polling.clone(),
        };
        let output = switches(vec![
            gauge(
                0,
                Readout::Channel {
                    source: com.id,
                    channel: 0,
                    unit: None,
                },
                false,
            ),
            gauge(
                1,
                Readout::Property {
                    source: native.id,
                    property: "temperature".into(),
                    unit: None,
                },
                false,
            ),
            gauge(
                2,
                Readout::Channel {
                    source: remote.id,
                    channel: 0,
                    unit: None,
                },
                false,
            ),
        ]);
        let output_id = output.id;
        let mut config = HubConfig::empty();
        config.sources = vec![com, native, remote];
        config.outputs = vec![output];
        let hub = HubRuntime::build(
            config,
            &f.native,
            &NoCredentials,
            Arc::new(MonotonicClock::default()),
        )
        .unwrap();
        let client = hub.client();
        client.connect(output_id).await.unwrap();
        let connection = client.connection(output_id).unwrap();
        until(|| (0..3).all(|id| connection.switch().unwrap().value(id).is_ok())).await;
        assert_eq!(connection.switch().unwrap().value(0).unwrap(), 0.0);
        assert!(connection.switch().unwrap().value(1).unwrap().is_finite());
        assert_eq!(connection.switch().unwrap().value(2).unwrap(), 42.0);
        drop(connection);
        client.close();
        hub.shutdown().await.unwrap();
    }
    server.abort();
}

#[tokio::test]
async fn corrupt_vendor_stdout_retires_parent_transport_without_consuming_later_acknowledgements() {
    let Some(f) = Fixture::load() else {
        return;
    };
    for architecture in [Bitness::X86, Bitness::X64] {
        for frame in [
            "vendor wrote a diagnostic into stdout".to_string(),
            json!({"ok":true,"result":{"protocol":1,"id":999,"value":"wrong request","error":null,
                "connection":{"deviceType":"switch","interfaceVersion":3,"method":"async","ownsConnection":true,"uncertain":false,"ready":true}}}).to_string(),
            r#"{"ok":true,"result":{"protocol":1,"id":1,"id":2,"value":true,"error":null,"connection":{"deviceType":"switch","interfaceVersion":3,"method":"async","ownsConnection":true,"uncertain":false,"ready":true}}}"#.into(),
        ] {
            f.clear("Other", json!({}));
            let cfg = f.source("Other", DeviceType::Switch, architecture);
            let mut backend = f.backend(&cfg, Vec::new());
            backend.connect().await.unwrap();
            let pid = f.pid("Other");
            f.state("Other", json!({"rawReplyMember":"Name","rawFrame":frame}));
            let error = backend.read("name".into(), Values::new()).await.unwrap_err();
            assert!(error.transport_lost);
            assert_eq!(error.kind, ErrorKind::Transient);
            until(|| !alive(pid)).await;
            assert_eq!(backend.read("name".into(), Values::new()).await.unwrap_err().kind, ErrorKind::Disconnected);
        }
    }
}

#[tokio::test]
async fn incomplete_async_disconnect_is_bounded_and_cannot_replay_after_reset() {
    let Some(f) = Fixture::load() else {
        return;
    };
    for architecture in [Bitness::X86, Bitness::X64] {
        f.clear("Switch", json!({}));
        let mut cfg = f.source("Switch", DeviceType::Switch, architecture);
        cfg.polling.request_timeout_seconds = 1.0;
        let mut backend = f.backend(&cfg, Vec::new());
        backend.connect().await.unwrap();
        let pid = f.pid("Switch");
        f.state("Switch", json!({"connectingForever":true}));
        let error = tokio::time::timeout(Duration::from_secs(2), backend.disconnect())
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Uncertain);
        until(|| !alive(pid)).await;
        assert!(backend.connection_info().unwrap().uncertain);
        f.state("Switch", json!({}));
        assert_eq!(
            backend.connect().await.unwrap_err().kind,
            ErrorKind::Uncertain
        );
        assert_eq!(
            backend.disconnect().await.unwrap_err().kind,
            ErrorKind::Uncertain
        );
        assert_eq!(f.count("Switch", "Disconnect"), 1);
    }
}
