use regain_hub::{
    alpaca::SampleType,
    config::{HubConfig, OutputConfig, Readout, SourceBackend, SwitchChannel, VirtualDevice},
    factory::{CredentialProvider, NoCredentials, build_sources, source_plans},
    native::NativeRuntime,
    safety::MonotonicClock,
    source::{ErrorKind, SourceError},
};
use reqwest::header::HeaderValue;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use uuid::Uuid;

fn weather() -> HubConfig {
    serde_json::from_str(include_str!("../examples/mixed-weather.json")).unwrap()
}
fn safety() -> HubConfig {
    serde_json::from_str(include_str!("../examples/two-source-safety.json")).unwrap()
}
fn gauge(number: u32, readout: Readout) -> SwitchChannel {
    SwitchChannel {
        id: Uuid::new_v4(),
        number,
        label: format!("Gauge {number}"),
        readout,
        writable: false,
        minimum: 0.0,
        maximum: 100.0,
        step: 1.0,
        units: "".into(),
    }
}
fn switch(number: u32, channels: Vec<SwitchChannel>) -> OutputConfig {
    OutputConfig {
        id: Uuid::new_v4(),
        number,
        label: format!("Switch {number}"),
        device: VirtualDevice::Switch { channels },
    }
}

#[test]
fn wheel_outputs_share_one_typed_metadata_plan_with_scalar_position_readouts() {
    use regain_hub::config::DeviceType;
    let mut config = HubConfig::empty();
    let mut source = weather().sources[0].clone();
    let SourceBackend::Alpaca { device_type, .. } = &mut source.backend else {
        panic!()
    };
    *device_type = DeviceType::FilterWheel;
    let source_id = source.id;
    config.sources.push(source);
    for number in [4, 9] {
        config.outputs.push(OutputConfig {
            id: Uuid::new_v4(),
            number,
            label: format!("Wheel {number}"),
            device: VirtualDevice::Proxy {
                source: source_id,
                device_type: DeviceType::FilterWheel,
            },
        });
    }
    config.outputs.push(switch(
        0,
        vec![gauge(
            0,
            Readout::Property {
                source: source_id,
                property: "position".into(),
                unit: None,
            },
        )],
    ));
    let saved = config.clone();
    let plans = source_plans(&config).unwrap();
    assert_eq!(plans.len(), 1);
    let samples = &plans[&source_id].samples;
    assert_eq!(samples.len(), 3);
    assert!(matches!(
        samples
            .iter()
            .find(|sample| sample.key == "names")
            .unwrap()
            .value_type,
        SampleType::Strings
    ));
    assert!(matches!(
        samples
            .iter()
            .find(|sample| sample.key == "focusoffsets")
            .unwrap()
            .value_type,
        SampleType::Int32s
    ));
    assert!(matches!(
        samples
            .iter()
            .find(|sample| sample.key == "position")
            .unwrap()
            .value_type,
        SampleType::Number
    ));
    assert!(
        samples
            .iter()
            .all(|sample| sample.parameters.is_empty() && sample.sensor_age.is_none())
    );
    assert_eq!(config, saved);
}

#[test]
fn panel_outputs_share_one_typed_poll_plan_with_combined_brightness_gauges() {
    use regain_hub::config::DeviceType;
    let mut config = HubConfig::empty();
    let mut source = weather().sources[0].clone();
    let SourceBackend::Alpaca { device_type, .. } = &mut source.backend else {
        panic!()
    };
    *device_type = DeviceType::CoverCalibrator;
    let source_id = source.id;
    config.sources.push(source);
    for number in [4, 17] {
        config.outputs.push(OutputConfig {
            id: Uuid::new_v4(),
            number,
            label: format!("Panel {number}"),
            device: VirtualDevice::Proxy {
                source: source_id,
                device_type: DeviceType::CoverCalibrator,
            },
        });
    }
    config.outputs.push(switch(
        0,
        vec![gauge(
            0,
            Readout::Property {
                source: source_id,
                property: "brightness".into(),
                unit: None,
            },
        )],
    ));
    let plans = source_plans(&config).unwrap();
    assert_eq!(plans.len(), 1);
    let samples = &plans[&source_id].samples;
    assert_eq!(samples.len(), 6);
    for property in regain_hub::covercalibrator::CoverCalibratorProperty::ALL {
        let sample = samples
            .iter()
            .find(|sample| sample.key == property.member())
            .unwrap();
        assert_eq!(sample.member, property.member());
        assert!(sample.parameters.is_empty());
        assert!(if property.value_type() == "boolean" {
            matches!(sample.value_type, SampleType::Boolean)
        } else {
            matches!(sample.value_type, SampleType::Number)
        });
    }
}

#[test]
fn typed_properties_count_toward_the_combined_poll_limit_after_deduplication() {
    use regain_hub::config::DeviceType;
    for (device_type, typed_count) in [
        (DeviceType::Focuser, 9),
        (DeviceType::Rotator, 7),
        (DeviceType::FilterWheel, 3),
    ] {
        let mut config = HubConfig::empty();
        let mut source = weather().sources[0].clone();
        let SourceBackend::Alpaca {
            device_type: imported_type,
            ..
        } = &mut source.backend
        else {
            panic!()
        };
        *imported_type = device_type;
        let source_id = source.id;
        config.sources.push(source);
        let count = regain_hub::source::MAX_SAMPLE_KEYS - typed_count;
        let channels = (0..count as u32)
            .map(|number| {
                gauge(
                    number,
                    Readout::Property {
                        source: source_id,
                        property: format!(
                            "custom{}{}{}",
                            char::from(b'a' + (number / 676) as u8),
                            char::from(b'a' + (number / 26 % 26) as u8),
                            char::from(b'a' + (number % 26) as u8)
                        ),
                        unit: None,
                    },
                )
            })
            .collect();
        config.outputs.push(switch(0, channels));
        config.outputs.push(OutputConfig {
            id: Uuid::new_v4(),
            number: 0,
            label: "Typed output".into(),
            device: VirtualDevice::Proxy {
                source: source_id,
                device_type,
            },
        });
        assert!(
            config.validate().is_empty(),
            "{:?}",
            config.validate().into_iter().take(3).collect::<Vec<_>>()
        );
        assert_eq!(
            source_plans(&config).unwrap()[&source_id].samples.len(),
            regain_hub::source::MAX_SAMPLE_KEYS
        );
        let VirtualDevice::Switch { channels } = &mut config.outputs[0].device else {
            panic!()
        };
        channels.push(gauge(
            count as u32,
            Readout::Property {
                source: source_id,
                property: "extra".into(),
                unit: None,
            },
        ));
        assert!(config.validate().is_empty());
        let before = config.clone();
        assert!(
            source_plans(&config)
                .err()
                .unwrap()
                .iter()
                .any(|error| error.code == "sample")
        );
        assert_eq!(config, before);
    }
}

#[test]
fn readouts_are_deduplicated_across_outputs_without_losing_weather_age_requests() {
    let mut config = weather();
    let source = config.sources[0].id;
    config.outputs.push(switch(
        0,
        vec![gauge(
            0,
            Readout::Property {
                source,
                property: "temperature".into(),
                unit: None,
            },
        )],
    ));
    let before = config.clone();
    let plan = source_plans(&config).unwrap();
    assert_eq!(config, before);
    let samples = &plan[&source].samples;
    assert_eq!(
        samples
            .iter()
            .map(|s| s.member.as_str())
            .collect::<Vec<_>>(),
        vec!["pressure", "temperature"]
    );
    for sample in samples {
        assert_eq!(sample.sensor_age.as_deref(), Some(sample.member.as_str()));
        assert!(matches!(sample.value_type, SampleType::Number));
    }
    let native = &plan[&config.sources[1].id].samples;
    assert_eq!(native.len(), 1);
    assert_eq!(native[0].member, "temperature");
    assert!(native[0].sensor_age.is_none());
    config.outputs.reverse();
    let reordered = source_plans(&config).unwrap();
    assert_eq!(
        samples.iter().map(|s| &s.key).collect::<Vec<_>>(),
        reordered[&source]
            .samples
            .iter()
            .map(|s| &s.key)
            .collect::<Vec<_>>()
    );
}

#[test]
fn fastest_enabled_membership_sets_shared_polling_without_changing_its_policy() {
    let mut config = safety();
    let VirtualDevice::Safety { members } = &mut config.outputs[0].device else {
        unreachable!()
    };
    members[0].policy.confirmation_seconds = 10.0;
    let mut second = config.outputs[0].clone();
    second.id = Uuid::new_v4();
    second.number = 1;
    let VirtualDevice::Safety { members } = &mut second.device else {
        unreachable!()
    };
    members[0].enabled = false;
    members[0].policy.confirmation_seconds = 0.1;
    config.outputs.push(second);
    let plan = source_plans(&config).unwrap();
    let first = &plan[&config.sources[0].id];
    assert_eq!(first.source.polling.poll_seconds, 10.0);
    assert_eq!(config.sources[0].polling.poll_seconds, 30.0);
    assert_eq!(
        plan[&config.sources[1].id].source.polling.poll_seconds,
        30.0
    );
    assert_eq!(first.samples.len(), 1);
    assert!(matches!(first.samples[0].value_type, SampleType::Boolean));
    assert!(first.samples[0].sensor_age.is_none());
    assert_eq!(first.samples[0].key, "issafe");
}

#[test]
fn safety_gauges_share_one_boolean_attempt_and_cannot_add_unrelated_properties() {
    let mut config = safety();
    config.outputs.push(switch(
        0,
        vec![gauge(
            0,
            Readout::Property {
                source: config.sources[0].id,
                property: "issafe".into(),
                unit: None,
            },
        )],
    ));
    let plan = source_plans(&config).unwrap();
    assert_eq!(plan[&config.sources[0].id].samples.len(), 1);
    let VirtualDevice::Switch { channels } = &mut config.outputs[1].device else {
        unreachable!()
    };
    let Readout::Property { property, .. } = &mut channels[0].readout else {
        unreachable!()
    };
    *property = "temperature".into();
    let errors = source_plans(&config).err().unwrap();
    assert_eq!(errors[0].code, "sample");
}

#[test]
fn source_sample_limit_applies_to_the_union_of_all_outputs() {
    let mut config: HubConfig =
        serde_json::from_str(include_str!("../examples/mixed-switch.json")).unwrap();
    let source = config.sources[0].id;
    config.outputs = (0..2)
        .map(|output| {
            switch(
                output,
                (0..513)
                    .map(|number| {
                        gauge(
                            number,
                            Readout::Channel {
                                source,
                                channel: output * 513 + number,
                                unit: None,
                            },
                        )
                    })
                    .collect(),
            )
        })
        .collect();
    assert!(config.validate().is_empty());
    let errors = source_plans(&config).err().unwrap();
    assert_eq!(errors[0].code, "sample");
    assert_eq!(errors[0].path, "sources[0]");
}

struct Counter(AtomicUsize);
impl CredentialProvider for Counter {
    fn authorization(&self, _: &str) -> Result<HeaderValue, SourceError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Err(SourceError::new(
            ErrorKind::Unavailable,
            "Fixture credential unavailable",
        ))
    }
}

#[tokio::test]
async fn validate_before_credentials_and_fail_closed_when_no_provider_is_available() {
    let mut config = safety();
    let SourceBackend::Alpaca {
        credential_reference,
        ..
    } = &mut config.sources[0].backend
    else {
        unreachable!()
    };
    *credential_reference = Some("private-reference".into());
    let native = NativeRuntime {
        cameras: None,
        directory: "missing-worker-directory".into(),
        simulate: false,
        references: None,
    };
    let credentials = Counter(AtomicUsize::new(0));
    let clock = Arc::new(MonotonicClock::default());
    config.revision = Uuid::nil();
    assert!(build_sources(&config, &native, &credentials, clock.clone()).is_err());
    assert_eq!(credentials.0.load(Ordering::Relaxed), 0);
    config.revision = Uuid::new_v4();
    let errors = build_sources(&config, &native, &credentials, clock.clone())
        .err()
        .unwrap();
    assert_eq!(credentials.0.load(Ordering::Relaxed), 1);
    assert_eq!(errors[0].path, "sources[0].backend");
    assert!(
        !serde_json::to_string(&errors)
            .unwrap()
            .contains("private-reference")
    );
    assert!(build_sources(&config, &native, &NoCredentials, clock).is_err());
}
