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
    values["transferLogMode"] = json!("circular");
    values["coolerMaxPowerChangePer5Seconds"] = json!(25.0);
    values["preExposureReconnectAttempts"] = json!(3);
    values
}

#[test]
fn cooler_power_change_is_finite_and_bounded_and_logging_modes_are_validated() {
    for value in [1.0, 25.0, 100.0] {
        RecoveryOptions {
            cooler_max_power_change_per_5_seconds: value,
            ..Default::default()
        }
        .validate()
        .unwrap();
    }
    for value in [0.0, -1.0, 100.0001, f64::NAN, f64::INFINITY] {
        assert!(
            RecoveryOptions {
                cooler_max_power_change_per_5_seconds: value,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }
    for invalid in [json!(0), json!(false), Value::Null] {
        assert!(
            serde_json::from_value::<RecoveryOptions>(json!({"transferLogMode":invalid})).is_err()
        );
    }
    for invalid in ["false", "", "unknown"] {
        let options: RecoveryOptions =
            serde_json::from_value(json!({"transferLogMode":invalid})).unwrap();
        assert!(options.validate().is_err());
    }
    let options: RecoveryOptions = serde_json::from_value(
        json!({"transferLogMode":"off","coolerMaxPowerChangePer5Seconds":5}),
    )
    .unwrap();
    assert_eq!(options.transfer_log_mode, "off");
    assert_eq!(options.cooler_max_power_change_per_5_seconds, 5.0);
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
fn pre_exposure_reconnect_attempts_have_independent_integer_bounds() {
    for attempts in [1, 3, 10] {
        let options: RecoveryOptions = serde_json::from_value(json!({
            "maxRetries":0, "preExposureReconnectAttempts":attempts
        }))
        .unwrap();
        options.validate().unwrap();
        assert_eq!(options.max_retries, 0);
        assert_eq!(options.pre_exposure_reconnect_attempts, attempts);
    }
    for invalid in [
        json!(0),
        json!(11),
        json!(-1),
        json!(1.5),
        json!(true),
        json!("3"),
        Value::Null,
    ] {
        assert!(
            serde_json::from_value::<RecoveryOptions>(json!({
                "preExposureReconnectAttempts":invalid
            }))
            .map(|options| options.validate().is_err())
            .unwrap_or(true)
        );
    }
    let schema = RecoveryOptions::schema();
    let field = &schema["properties"]["preExposureReconnectAttempts"];
    assert_eq!(field["default"], 3);
    assert_eq!(field["minimum"], 1);
    assert_eq!(field["maximum"], 10);
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
    assert_eq!(schema["properties"].as_object().unwrap().len(), 18);
    for (key, default) in defaults.as_object().unwrap() {
        let property = &schema["properties"][key];
        assert_eq!(&property["default"], default);
        assert_eq!(property["x-regain"]["key"], *key);
        assert_eq!(
            property["x-regain"]["apply"],
            if matches!(
                key.as_str(),
                "transferLogMode" | "coolerMaxPowerChangePer5Seconds"
            ) {
                "live"
            } else {
                "reconnect"
            }
        );
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
