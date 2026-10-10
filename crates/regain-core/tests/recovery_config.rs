use regain_core::RecoveryOptions;
use serde_json::{Value, json};

// Independent fixture from the shipped 0.5 profile format. Keep this literal:
// regenerating it from the declarations would conceal a compatibility change.
fn legacy_defaults() -> Value {
    json!({
        "maxRetries":3, "maximumRetryExposureSeconds":30.0,
        "reconnectDelaySeconds":5.0, "commandTimeoutSeconds":15.0,
        "downloadTimeoutSeconds":60.0, "exposureGraceSeconds":30.0,
        "coolingTimeoutSeconds":300.0, "temperatureToleranceC":2.0,
        "coolingStableSamples":3, "coolingSampleSeconds":2.0,
        "readyFrameDownloadRetries":2, "directReadRetries":2,
        "usbResetAfterFailures":0, "usbPortCycle":false
    })
}

fn current_defaults() -> Value {
    let mut values = legacy_defaults();
    values["directReadChunkKiB"] = json!(1024);
    values
}

#[test]
fn legacy_defaults_sparse_profiles_and_public_paths_remain_compatible() {
    let defaults: regain_core::model::RecoveryOptions = RecoveryOptions::default();
    assert_eq!(serde_json::to_value(defaults).unwrap(), current_defaults());
    let sparse: RecoveryOptions = serde_json::from_value(json!({
        "maxRetries":0, "directReadRetries":5, "usbPortCycle":true,
        "unknownLegacyExtension":"preserved loader behavior"
    }))
    .unwrap();
    let mut expected = current_defaults();
    expected["maxRetries"] = json!(0);
    expected["directReadRetries"] = json!(5);
    expected["usbPortCycle"] = json!(true);
    assert_eq!(serde_json::to_value(&sparse).unwrap(), expected);
    sparse.validate().unwrap();
    let restored: RecoveryOptions = serde_json::from_value(expected).unwrap();
    assert_eq!(restored, sparse);
}

#[test]
fn legacy_integer_boundaries_and_input_types_remain_compatible() {
    for (key, min, max) in [
        ("maxRetries", 0, 20),
        ("readyFrameDownloadRetries", 0, 5),
        ("directReadRetries", 0, 5),
        ("usbResetAfterFailures", 0, 20),
        ("coolingStableSamples", 1, 60),
    ] {
        for v in [min, max] {
            let mut value = legacy_defaults();
            value[key] = json!(v);
            serde_json::from_value::<RecoveryOptions>(value)
                .unwrap()
                .validate()
                .unwrap();
        }
        for v in [json!(max + 1), json!(min - 1)] {
            let mut value = legacy_defaults();
            value[key] = v;
            assert!(
                serde_json::from_value::<RecoveryOptions>(value)
                    .map(|o| o.validate().is_err())
                    .unwrap_or(true),
                "{key}"
            );
        }
        for v in [json!(1.0), json!(true), json!("1"), Value::Null] {
            let mut value = legacy_defaults();
            value[key] = v;
            assert!(
                serde_json::from_value::<RecoveryOptions>(value).is_err(),
                "{key}"
            );
        }
    }
}

#[test]
fn legacy_float_boundaries_including_nonfinite_values_remain_compatible() {
    type Field = fn(&mut RecoveryOptions) -> &mut f64;
    let positive: [Field; 7] = [
        |o| &mut o.reconnect_delay_seconds,
        |o| &mut o.command_timeout_seconds,
        |o| &mut o.download_timeout_seconds,
        |o| &mut o.exposure_grace_seconds,
        |o| &mut o.cooling_timeout_seconds,
        |o| &mut o.temperature_tolerance_c,
        |o| &mut o.cooling_sample_seconds,
    ];
    for field in positive {
        for v in [f64::MIN_POSITIVE, 0.000001, 3600.0] {
            let mut o = RecoveryOptions::default();
            *field(&mut o) = v;
            o.validate().unwrap();
        }
        for v in [
            -0.0,
            0.0,
            -1.0,
            3600.000001,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ] {
            let mut o = RecoveryOptions::default();
            *field(&mut o) = v;
            let error = o.validate().unwrap_err();
            assert!(
                matches!(
                    error.downcast_ref::<regain_core::Failure>(),
                    Some(regain_core::Failure::Invalid(_))
                ),
                "{v}: {error}"
            );
        }
    }
    for v in [-0.0, 0.0, f64::MIN_POSITIVE, 86400.0] {
        RecoveryOptions {
            maximum_retry_exposure_seconds: v,
            ..Default::default()
        }
        .validate()
        .unwrap();
    }
    for v in [
        -1.0,
        86400.000001,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
    ] {
        assert!(
            RecoveryOptions {
                maximum_retry_exposure_seconds: v,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }
}

#[test]
fn descriptor_defaults_and_serialized_keys_match_the_shipped_profile() {
    let schema = RecoveryOptions::schema();
    let defaults = current_defaults();
    assert_eq!(schema["default"], defaults);
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["properties"].as_object().unwrap().len(), 15);
    for (key, default) in defaults.as_object().unwrap() {
        let property = &schema["properties"][key];
        assert_eq!(&property["default"], default);
        assert_eq!(property["x-regain"]["key"], *key);
        assert_eq!(property["x-regain"]["apply"], "reconnect");
        assert!(!property["description"].as_str().unwrap().is_empty());
    }
    assert_eq!(
        schema["properties"]["reconnectDelaySeconds"]["exclusiveMinimum"],
        0.0
    );
    assert_eq!(
        schema["properties"]["maximumRetryExposureSeconds"]["minimum"],
        0.0
    );
}
