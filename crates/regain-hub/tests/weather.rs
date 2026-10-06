use regain_hub::{
    config::{Measurement, Readout, WeatherMetric as M},
    source::{ErrorKind, SourceError, SourceSnapshot, Values},
    weather::WeatherEngine,
};
use serde_json::json;
use std::{collections::BTreeMap, time::Duration};
use uuid::Uuid;
fn t(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}
fn readout(source: Uuid, property: &str) -> Readout {
    Readout::Property {
        source,
        property: property.into(),
        unit: None,
    }
}
fn measurement(sources: Vec<Readout>, average: f64) -> Measurement {
    Measurement {
        sources,
        maximum_age_seconds: 60.0,
        average_seconds: average,
    }
}
fn snapshot(source: Uuid, sequence: u64, seconds: u64, values: Values) -> SourceSnapshot {
    SourceSnapshot {
        source,
        revision: Uuid::from_u128(1),
        generation: Uuid::from_u128(source.as_u128() + 100),
        sequence,
        transport_connected: true,
        write_uncertain: false,
        connection_info: None,
        simulated: false,
        simulation: None,
        lease_count: 1,
        values,
        sample_errors: BTreeMap::new(),
        sample_ages_seconds: BTreeMap::new(),
        sample_started_seconds: BTreeMap::new(),
        sample_sequences: BTreeMap::new(),
        completed_passes: 0,
        sampled_at_seconds: Some(seconds as f64),
        error: None,
    }
}
fn temperature(source: Uuid, sequence: u64, seconds: u64, value: f64) -> SourceSnapshot {
    snapshot(
        source,
        sequence,
        seconds,
        Values::from([("temperature".into(), json!(value))]),
    )
}

#[test]
fn fallback_is_per_measurement_and_expired_samples_never_become_fresh_again() {
    let a = Uuid::from_u128(10);
    let b = Uuid::from_u128(20);
    let mut engine = WeatherEngine::new(BTreeMap::from([
        (
            M::Temperature,
            measurement(
                vec![readout(a, "temperature"), readout(b, "temperature")],
                0.0,
            ),
        ),
        (M::Pressure, measurement(vec![readout(a, "pressure")], 0.0)),
    ]));
    let mut first = temperature(a, 1, 0, 10.0);
    first.values.insert("pressure".into(), json!(1000));
    engine.observe(first, t(0));
    engine.observe(temperature(b, 1, 10, 20.0), t(10));
    assert_eq!(engine.read(M::Temperature, t(10)).unwrap().source, a);
    let mut failed = snapshot(a, 2, 20, Values::from([("pressure".into(), json!(1001))]));
    failed.sample_errors.insert(
        "temperature".into(),
        SourceError::new(ErrorKind::Unavailable, "sensor fault"),
    );
    engine.observe(failed, t(20));
    assert_eq!(engine.read(M::Temperature, t(20)).unwrap().source, b);
    assert_eq!(engine.read(M::Pressure, t(20)).unwrap().value, 1001.0);
    assert_eq!(
        engine.read(M::Temperature, t(70)).unwrap_err().kind,
        ErrorKind::Unavailable
    );
    assert_eq!(
        engine.time_since_last_update("Temperature", t(70)).unwrap(),
        60.0
    );
    assert_eq!(engine.time_since_last_update("", t(70)).unwrap(), 50.0);
    assert_eq!(
        engine
            .time_since_last_update("humidity", t(70))
            .unwrap_err()
            .kind,
        ErrorKind::Unsupported
    );
    assert_eq!(
        engine
            .time_since_last_update("typo", t(70))
            .unwrap_err()
            .kind,
        ErrorKind::InvalidValue
    );
}

#[test]
fn averages_are_time_weighted_getters_do_not_add_samples_and_source_changes_clear_history() {
    let a = Uuid::from_u128(10);
    let b = Uuid::from_u128(20);
    let mut engine = WeatherEngine::new(BTreeMap::from([(
        M::Temperature,
        measurement(
            vec![readout(a, "temperature"), readout(b, "temperature")],
            30.0,
        ),
    )]));
    engine.observe(temperature(a, 1, 0, 0.0), t(0));
    engine.observe(temperature(a, 2, 10, 20.0), t(10));
    let reading = engine.read(M::Temperature, t(20)).unwrap();
    assert_eq!(reading.value, 10.0);
    assert_eq!(reading.sample_count, 2);
    for _ in 0..20 {
        assert_eq!(engine.read(M::Temperature, t(20)).unwrap().sample_count, 2);
    }
    engine.observe(temperature(b, 1, 20, 40.0), t(20));
    let mut failed = temperature(a, 3, 20, 99.0);
    failed.error = Some(SourceError::transient());
    engine.observe(failed, t(20));
    let reading = engine.read(M::Temperature, t(20)).unwrap();
    assert_eq!(reading.value, 40.0);
    assert_eq!(reading.sample_count, 1);
    engine.observe(temperature(a, 4, 30, 5.0), t(30));
    assert_eq!(engine.read(M::Temperature, t(30)).unwrap().value, 5.0);
    engine.set_average_period_hours(0.0).unwrap();
    assert_eq!(engine.average_period_hours(), 0.0);
    assert!(engine.set_average_period_hours(f64::NAN).is_err());
    assert!(engine.set_average_period_hours(-1.0).is_err());
}

#[test]
fn sensor_age_counts_toward_expiry_and_invalid_clock_or_generation_cannot_reuse_history() {
    let a = Uuid::from_u128(10);
    let mut engine = WeatherEngine::new(BTreeMap::from([(
        M::Temperature,
        measurement(vec![readout(a, "temperature")], 30.0),
    )]));
    let mut old = temperature(a, 1, 10, 5.0);
    old.sample_ages_seconds.insert("temperature".into(), 55.0);
    engine.observe(old, t(10));
    assert_eq!(
        engine.read(M::Temperature, t(14)).unwrap().age_seconds,
        59.0
    );
    assert!(engine.read(M::Temperature, t(15)).is_err());
    let mut fresh = temperature(a, 1, 20, 30.0);
    fresh.generation = Uuid::new_v4();
    engine.observe(fresh, t(20));
    assert_eq!(engine.read(M::Temperature, t(20)).unwrap().sample_count, 1);
    assert!(engine.read(M::Temperature, t(19)).is_err());
    engine.observe(temperature(a, 2, 21, 40.0), t(21));
    assert!(engine.read(M::Temperature, t(21)).is_err());
}

#[test]
fn circular_wind_average_and_calm_do_not_produce_a_false_southerly_wind() {
    let a = Uuid::from_u128(10);
    let mut engine = WeatherEngine::new(BTreeMap::from([
        (
            M::WindDirection,
            measurement(vec![readout(a, "winddirection")], 30.0),
        ),
        (
            M::WindSpeed,
            measurement(vec![readout(a, "windspeed")], 30.0),
        ),
        (M::WindGust, measurement(vec![readout(a, "windgust")], 30.0)),
    ]));
    for (sequence, second, direction, gust) in [(1, 0, 350, 12), (2, 10, 10, 8)] {
        engine.observe(
            snapshot(
                a,
                sequence,
                second,
                Values::from([
                    ("winddirection".into(), json!(direction)),
                    ("windspeed".into(), json!(5)),
                    ("windgust".into(), json!(gust)),
                ]),
            ),
            t(second),
        );
    }
    let direction = engine.read(M::WindDirection, t(20)).unwrap().value;
    assert!(!(1e-6..=359.999999).contains(&direction));
    assert_eq!(
        engine.read(M::WindGust, t(20)).unwrap().value,
        8.0,
        "source gust already has ASCOM peak semantics; do not average it away"
    );
    engine.set_average_period_hours(0.0).unwrap();
    engine.observe(
        snapshot(
            a,
            3,
            30,
            Values::from([
                ("winddirection".into(), json!(120)),
                ("windspeed".into(), json!(0)),
                ("windgust".into(), json!(8)),
            ]),
        ),
        t(30),
    );
    assert_eq!(engine.read(M::WindDirection, t(30)).unwrap().value, 0.0);
}

#[test]
fn weather_validation_requires_units_paired_sensors_and_one_average_period() {
    use regain_hub::config::{DeviceType, HubConfig, SourceBackend, VirtualDevice};
    let mut config: HubConfig =
        serde_json::from_str(include_str!("../examples/two-source-safety.json")).unwrap();
    let source = config.sources[0].id;
    if let SourceBackend::Alpaca { device_type, .. } = &mut config.sources[0].backend {
        *device_type = DeviceType::ObservingConditions;
    }
    config.outputs[0].device = VirtualDevice::Weather {
        measurements: BTreeMap::from([(
            M::Temperature,
            measurement(vec![readout(source, "temperature")], 0.0),
        )]),
    };
    assert!(config.validate().is_empty());
    let VirtualDevice::Weather { measurements } = &mut config.outputs[0].device else {
        unreachable!()
    };
    measurements.insert(
        M::Humidity,
        measurement(vec![readout(source, "humidity")], 10.0),
    );
    let errors = config.validate();
    assert!(
        errors
            .iter()
            .any(|error| error.message.contains("dew point"))
    );
    assert!(
        errors
            .iter()
            .any(|error| error.message.contains("one averaging"))
    );
    let VirtualDevice::Weather { measurements } = &mut config.outputs[0].device else {
        unreachable!()
    };
    measurements.remove(&M::Humidity);
    let Readout::Property { unit, .. } =
        &mut measurements.get_mut(&M::Temperature).unwrap().sources[0]
    else {
        unreachable!()
    };
    *unit = Some("°F".into());
    assert!(
        config
            .validate()
            .iter()
            .any(|error| error.message.contains("unit"))
    );
}

#[test]
fn malformed_scalar_types_do_not_become_measurements_and_large_finite_averages_remain_finite() {
    let a = Uuid::from_u128(10);
    let mut engine = WeatherEngine::new(BTreeMap::from([(
        M::Temperature,
        measurement(vec![readout(a, "temperature")], 30.0),
    )]));
    let mut bad = temperature(a, 1, 0, 0.0);
    bad.values.insert("temperature".into(), json!(true));
    engine.observe(bad, t(0));
    assert!(engine.read(M::Temperature, t(0)).is_err());
    engine.observe(temperature(a, 2, 10, 1e308), t(10));
    engine.observe(temperature(a, 3, 20, 1e308), t(20));
    assert_eq!(engine.read(M::Temperature, t(30)).unwrap().value, 1e308);
}
