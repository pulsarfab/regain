//! Actual HTTP publication over the private host, with loopback inputs or an
//! explicitly simulated production OFP2 worker. Never opens physical hardware.
use super::*;
use std::sync::atomic::Ordering::SeqCst;

fn commands(upstream: &AccessoryUpstream) -> Vec<AccessoryWrite> {
    upstream
        .writes
        .lock()
        .unwrap()
        .iter()
        .filter(|(member, _)| !matches!(member.as_str(), "connected" | "connect" | "disconnect"))
        .cloned()
        .collect()
}

async fn page(f: &Fixture, slot: u32) -> (StatusCode, String) {
    let response = f
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/setup/v1/covercalibrator/{slot}/setup"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, std::str::from_utf8(&bytes).unwrap().into())
}

#[tokio::test]
async fn panel_publication_preserves_dynamic_identity_shared_clients_and_independent_completion() {
    for version in [1, 2] {
        let upstream = AccessoryUpstream::covercalibrator(version).await;
        let other = AccessoryUpstream::covercalibrator(version).await;
        let mut config = upstream.config(&[4, 17]);
        let second = other.config(&[12]);
        config.sources.extend(second.sources);
        config.outputs.extend(second.outputs);
        let f = Fixture::from_config(config).await;
        let devices = f.ok("GET", "/management/v1/configureddevices", "").await;
        assert_eq!(devices.as_array().unwrap().len(), 3);
        for output in &f.config.outputs {
            let device = devices
                .as_array()
                .unwrap()
                .iter()
                .find(|device| device["UniqueID"] == json!(output.id))
                .unwrap();
            assert_eq!(device["DeviceNumber"], output.number);
            assert_eq!(device["DeviceType"], "CoverCalibrator");
            assert_eq!(device["DeviceName"], output.label);
        }
        assert_eq!(
            f.hub
                .source_snapshot(upstream.source.id)
                .unwrap()
                .lease_count,
            0
        );
        assert!(upstream.writes.lock().unwrap().is_empty());
        assert_eq!(
            f.ok("GET", "/api/v1/covercalibrator/4/interfaceversion", "")
                .await,
            2
        );
        assert_eq!(
            f.call("GET", "/api/v1/covercalibrator/4/brightness", "ClientID=1")
                .await["ErrorNumber"],
            0x407
        );
        assert!(page(&f, 4).await.1.contains("hub.mjs"));
        assert_eq!(page(&f, 5).await.0, StatusCode::NOT_FOUND);
        assert_eq!(
            request(&f.router, "GET", "/api/v1/covercalibrator/5/connected", "")
                .await
                .0,
            StatusCode::NOT_FOUND
        );

        // A standalone local panel retains its setup and identity at slot zero.
        f.server
            .flatpanel
            .configure(json!({"serial":"PRIVATE-OFP2"}))
            .await
            .unwrap();
        assert_eq!(
            f.ok("GET", "/management/v1/configureddevices", "")
                .await
                .as_array()
                .unwrap()
                .len(),
            4
        );
        assert_eq!(
            f.ok("GET", "/api/v1/covercalibrator/0/connected", "ClientID=99")
                .await,
            false
        );
        assert!(page(&f, 0).await.1.contains("flatpanel.js"));

        f.ok(
            "PUT",
            "/api/v1/covercalibrator/4/connected",
            "ClientID=1&Connected=true",
        )
        .await;
        f.ok("PUT", "/api/v1/covercalibrator/17/connect", "ClientID=2")
            .await;
        eventually(async || {
            f.ok("GET", "/api/v1/covercalibrator/17/connecting", "ClientID=2")
                .await
                == false
        })
        .await;
        assert_eq!(
            f.ok("GET", "/api/v1/covercalibrator/17/connected", "ClientID=2")
                .await,
            true
        );
        assert_eq!(
            f.hub
                .source_snapshot(upstream.source.id)
                .unwrap()
                .lease_count,
            2
        );
        for (member, expected) in [
            ("brightness", json!(0)),
            ("maxbrightness", json!(4096)),
            ("coverstate", json!(1)),
            ("calibratorstate", json!(1)),
            ("covermoving", json!(false)),
            ("calibratorchanging", json!(false)),
        ] {
            assert_eq!(
                f.ok(
                    "GET",
                    &format!("/api/v1/covercalibrator/4/{member}"),
                    "ClientID=1"
                )
                .await,
                expected
            );
        }
        for value in ["-1", "4097", "2147483648", "-2147483649", "1.5", "bogus"] {
            assert_eq!(
                f.call(
                    "PUT",
                    "/api/v1/covercalibrator/4/calibratoron",
                    &format!("ClientID=1&Brightness={value}")
                )
                .await["ErrorNumber"],
                0x401
            );
        }
        for args in ["ClientID=1", "ClientID=1&Brightness=1&brightness=2"] {
            assert_eq!(
                f.call("PUT", "/api/v1/covercalibrator/4/calibratoron", args)
                    .await["ErrorNumber"],
                0x401
            );
        }
        for member in ["move", "halt", "brightness", "commandblind"] {
            assert_eq!(
                f.call(
                    "PUT",
                    &format!("/api/v1/covercalibrator/4/{member}"),
                    "ClientID=1&Brightness=1"
                )
                .await["ErrorNumber"],
                0x400
            );
        }
        assert!(commands(&upstream).is_empty());
        let accepted = f
            .call(
                "PUT",
                "/api/v1/covercalibrator/4/opencover",
                "ClientID=1&ClientTransactionID=87",
            )
            .await;
        assert_eq!(accepted["ErrorNumber"], 0);
        assert_eq!(accepted["ClientTransactionID"], 87);
        assert_eq!(
            f.ok(
                "GET",
                "/api/v1/covercalibrator/17/covermoving",
                "ClientID=2"
            )
            .await,
            true
        );
        // Imported drivers own preemption policy: closing while opening is not
        // rejected by a generic hub-wide motion rule.
        f.ok("PUT", "/api/v1/covercalibrator/17/closecover", "ClientID=2")
            .await;
        f.ok("PUT", "/api/v1/covercalibrator/17/haltcover", "ClientID=2")
            .await;
        assert_eq!(
            f.ok("GET", "/api/v1/covercalibrator/4/coverstate", "ClientID=1")
                .await,
            4
        );
        if version == 1 {
            assert_eq!(
                f.call("GET", "/api/v1/covercalibrator/4/covermoving", "ClientID=1")
                    .await["ErrorNumber"],
                0x402
            );
        } else {
            assert_eq!(
                f.ok("GET", "/api/v1/covercalibrator/4/covermoving", "ClientID=1")
                    .await,
                false
            );
        }
        f.ok(
            "PUT",
            "/api/v1/covercalibrator/4/calibratoron",
            "ClientID=1&Brightness=0",
        )
        .await;
        assert_eq!(
            f.ok("GET", "/api/v1/covercalibrator/17/brightness", "ClientID=2")
                .await,
            0
        );
        assert_eq!(
            f.ok(
                "GET",
                "/api/v1/covercalibrator/17/calibratorstate",
                "ClientID=2"
            )
            .await,
            2
        );
        assert_eq!(
            f.ok(
                "GET",
                "/api/v1/covercalibrator/17/calibratorchanging",
                "ClientID=2"
            )
            .await,
            true
        );
        upstream
            .values
            .lock()
            .unwrap()
            .insert("calibratorstate".into(), json!(3));
        upstream
            .values
            .lock()
            .unwrap()
            .insert("calibratorchanging".into(), json!(false));
        eventually(async || {
            let state = f.hub.source_snapshot(upstream.source.id).unwrap();
            state.values.get("calibratorstate") == Some(&json!(3))
                && state.values.get("maxbrightness") == Some(&json!(4096))
                && state.values.get("coverstate") == Some(&json!(4))
                && (version == 1 || state.values.get("calibratorchanging") == Some(&json!(false)))
        })
        .await;
        let state = f
            .ok(
                "GET",
                "/api/v1/covercalibrator/17/devicestate",
                "ClientID=2",
            )
            .await;
        let state = state.as_array().unwrap();
        assert!(
            state
                .iter()
                .any(|item| item["Name"] == "Brightness" && item["Value"] == 0)
        );
        assert!(
            state
                .iter()
                .any(|item| item["Name"] == "CalibratorChanging" && item["Value"] == false)
        );
        assert!(
            state
                .iter()
                .any(|item| item["Name"] == "CoverState" && item["Value"] == 4)
        );
        assert_eq!(
            state.iter().any(|item| item["Name"] == "CoverMoving"),
            version == 2
        );
        assert!(
            state
                .iter()
                .all(|item| item["Name"] != "MaxBrightness" && item["Name"] != "TimeStamp")
        );
        f.ok(
            "PUT",
            "/api/v1/covercalibrator/12/connected",
            "ClientID=3&Connected=true",
        )
        .await;
        assert_eq!(
            f.ok("GET", "/api/v1/covercalibrator/12/coverstate", "ClientID=3")
                .await,
            1
        );
        f.ok(
            "PUT",
            "/api/v1/covercalibrator/4/connected",
            "ClientID=1&Connected=false",
        )
        .await;
        assert!(upstream.connected.load(SeqCst));
        assert_eq!(
            f.ok(
                "GET",
                "/api/v1/covercalibrator/17/calibratorstate",
                "ClientID=2"
            )
            .await,
            3
        );
        f.ok(
            "PUT",
            "/api/v1/covercalibrator/17/calibratoroff",
            "ClientID=2",
        )
        .await;
        assert_eq!(
            f.ok("GET", "/api/v1/covercalibrator/17/brightness", "ClientID=2")
                .await,
            0
        );
        f.ok("PUT", "/api/v1/covercalibrator/17/disconnect", "ClientID=2")
            .await;
        eventually(async || !upstream.connected.load(SeqCst)).await;
        assert!(other.connected.load(SeqCst));
        let writes = upstream.writes.lock().unwrap().clone();
        assert!(
            writes
                .iter()
                .all(|(_, args)| args["ClientID"] == writes[0].1["ClientID"])
        );
        let transactions = writes
            .iter()
            .map(|(_, args)| args["ClientTransactionID"].clone())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(transactions.len(), writes.len());
        assert_eq!(
            commands(&upstream)
                .iter()
                .map(|(member, _)| member.as_str())
                .collect::<Vec<_>>(),
            [
                "opencover",
                "closecover",
                "haltcover",
                "calibratoron",
                "calibratoroff"
            ]
        );
        f.finish().await;
        upstream.finish().await;
        other.finish().await;
    }
}

#[tokio::test]
async fn panel_publication_validates_live_bounds_capabilities_and_modern_completion() {
    let upstream = AccessoryUpstream::covercalibrator(2).await;
    let f = Fixture::from_config(upstream.config(&[4])).await;
    f.ok(
        "PUT",
        "/api/v1/covercalibrator/4/connected",
        "ClientID=1&Connected=true",
    )
    .await;
    let original = upstream.values.lock().unwrap().clone();
    for (key, value, member) in [
        ("maxbrightness", json!(0), "maxbrightness"),
        ("maxbrightness", json!(2147483648_i64), "maxbrightness"),
        ("brightness", json!(1), "brightness"),
        ("brightness", json!(-1), "brightness"),
        ("coverstate", json!(6), "coverstate"),
        ("calibratorstate", json!(1.5), "calibratorstate"),
        ("covermoving", json!(0), "covermoving"),
        ("calibratorchanging", json!("false"), "calibratorchanging"),
    ] {
        *upstream.values.lock().unwrap() = original.clone();
        upstream.values.lock().unwrap().insert(key.into(), value);
        assert_eq!(
            f.call(
                "GET",
                &format!("/api/v1/covercalibrator/4/{member}"),
                "ClientID=1"
            )
            .await["ErrorNumber"],
            0x402
        );
    }
    *upstream.values.lock().unwrap() = original.clone();
    upstream.values.lock().unwrap().remove("covermoving");
    let missing = f
        .call("GET", "/api/v1/covercalibrator/4/covermoving", "ClientID=1")
        .await;
    assert_eq!(missing["ErrorNumber"], 0x400);
    assert!(!missing.to_string().contains("private detail"));
    *upstream.values.lock().unwrap() = original.clone();
    upstream
        .values
        .lock()
        .unwrap()
        .insert("maxbrightness".into(), json!(17));
    assert_eq!(
        f.call(
            "PUT",
            "/api/v1/covercalibrator/4/calibratoron",
            "ClientID=1&Brightness=18"
        )
        .await["ErrorNumber"],
        0x401
    );
    assert!(commands(&upstream).is_empty());
    f.ok(
        "PUT",
        "/api/v1/covercalibrator/4/calibratoron",
        "ClientID=1&Brightness=17",
    )
    .await;
    assert_eq!(commands(&upstream).len(), 1);
    upstream
        .values
        .lock()
        .unwrap()
        .insert("coverstate".into(), json!(0));
    for member in ["opencover", "closecover", "haltcover"] {
        assert_eq!(
            f.call(
                "PUT",
                &format!("/api/v1/covercalibrator/4/{member}"),
                "ClientID=1"
            )
            .await["ErrorNumber"],
            0x400
        );
    }
    // A cover-only device never invents an illuminator or a brightness range.
    *upstream.values.lock().unwrap() = original;
    upstream
        .values
        .lock()
        .unwrap()
        .insert("calibratorstate".into(), json!(0));
    upstream.values.lock().unwrap().remove("brightness");
    upstream.values.lock().unwrap().remove("maxbrightness");
    for member in ["brightness", "maxbrightness"] {
        assert_eq!(
            f.call(
                "GET",
                &format!("/api/v1/covercalibrator/4/{member}"),
                "ClientID=1"
            )
            .await["ErrorNumber"],
            0x400
        );
    }
    for (member, args) in [
        ("calibratoron", "ClientID=1&Brightness=0"),
        ("calibratoroff", "ClientID=1"),
    ] {
        assert_eq!(
            f.call("PUT", &format!("/api/v1/covercalibrator/4/{member}"), args)
                .await["ErrorNumber"],
            0x400
        );
    }
    assert_eq!(commands(&upstream).len(), 1);
    assert!(
        !f.hub
            .source_snapshot(upstream.source.id)
            .unwrap()
            .write_uncertain
    );
    f.finish().await;
    upstream.finish().await;
}

#[tokio::test]
async fn panel_publication_rejects_slot_zero_collision_without_connecting_equipment() {
    let upstream = AccessoryUpstream::covercalibrator(2).await;
    let f = Fixture::from_config(upstream.config(&[0])).await;
    assert!(page(&f, 0).await.1.contains("hub.mjs"));
    f.server
        .flatpanel
        .configure(json!({"serial":"PRIVATE-OFP2"}))
        .await
        .unwrap();
    assert_eq!(
        f.call("GET", "/management/v1/configureddevices", "").await["ErrorNumber"],
        0x401
    );
    for member in ["connected", "brightness"] {
        let reply = f
            .call(
                "GET",
                &format!("/api/v1/covercalibrator/0/{member}"),
                "ClientID=1&ClientTransactionID=92",
            )
            .await;
        assert_eq!(reply["ErrorNumber"], 0x401);
        assert_eq!(reply["ClientTransactionID"], 92);
    }
    assert_eq!(page(&f, 0).await.0, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        f.hub
            .source_snapshot(upstream.source.id)
            .unwrap()
            .lease_count,
        0
    );
    assert!(upstream.writes.lock().unwrap().is_empty());
    f.server.flatpanel.configure(json!({})).await.unwrap();
    assert_eq!(
        f.ok("GET", "/management/v1/configureddevices", "")
            .await
            .as_array()
            .unwrap()
            .len(),
        1
    );
    f.finish().await;
    upstream.finish().await;
}

#[tokio::test]
async fn panel_lost_command_reply_fences_siblings_without_replay_or_cleanup_actuation() {
    for version in [1, 2] {
        let upstream = AccessoryUpstream::covercalibrator(version).await;
        let f = Fixture::from_config(upstream.config(&[4, 17])).await;
        for (slot, client) in [(4, 1), (17, 2)] {
            f.ok(
                "PUT",
                &format!("/api/v1/covercalibrator/{slot}/connected"),
                &format!("ClientID={client}&Connected=true"),
            )
            .await;
        }
        upstream.lose_move_reply.store(true, SeqCst);
        let reply = f
            .call(
                "PUT",
                "/api/v1/covercalibrator/4/calibratoron",
                "ClientID=1&Brightness=17",
            )
            .await;
        assert_eq!(reply["ErrorNumber"], 0x500);
        assert_eq!(upstream.values.lock().unwrap()["brightness"], 17);
        for (member, args) in [
            ("calibratoroff", "ClientID=2"),
            ("haltcover", "ClientID=2"),
            ("closecover", "ClientID=2"),
            ("calibratoron", "ClientID=2&Brightness=17"),
        ] {
            let blocked = f
                .call("PUT", &format!("/api/v1/covercalibrator/17/{member}"), args)
                .await;
            assert_eq!(blocked["ErrorNumber"], 0x500);
            assert!(
                blocked["ErrorMessage"]
                    .as_str()
                    .unwrap()
                    .contains("do not replay")
            );
        }
        assert!(
            f.hub
                .source_snapshot(upstream.source.id)
                .unwrap()
                .write_uncertain
        );
        assert_eq!(commands(&upstream).len(), 1);
        assert_eq!(
            f.ok("GET", "/api/v1/covercalibrator/17/connected", "ClientID=2")
                .await,
            false
        );
        f.ok(
            "PUT",
            "/api/v1/covercalibrator/17/connected",
            "ClientID=2&Connected=true",
        )
        .await;
        assert_eq!(
            f.ok("GET", "/api/v1/covercalibrator/17/connected", "ClientID=2")
                .await,
            false
        );
        f.finish().await;
        assert_eq!(commands(&upstream).len(), 1);
        upstream.finish().await;
    }
}

#[tokio::test]
async fn panel_native_ofp2_worker_publishes_light_and_cover_through_shared_http_clients() {
    use regain_hub::config::{DeviceType, NativeDevice, OutputConfig, SourceBackend, SourceConfig};
    let Some(workers) = std::env::var_os("REGAIN_TEST_WORKERS") else {
        eprintln!(
            "Native panel publication requires REGAIN_TEST_WORKERS pointing to built production workers"
        );
        return;
    };
    let mut config = HubConfig::empty();
    let source = uuid::Uuid::new_v4();
    config.sources.push(SourceConfig {
        id: source,
        label: "Explicit OFP2 simulation".into(),
        polling: regain_hub::parameters::PollPolicy {
            poll_seconds: 0.2,
            request_timeout_seconds: 5.0,
            ..Default::default()
        },
        backend: SourceBackend::Native {
            device: NativeDevice::Ofp2,
            identity: "SIM-OFP2".into(),
            filter_wheel: None,
        },
    });
    for number in [4, 17] {
        config.outputs.push(OutputConfig {
            id: uuid::Uuid::new_v4(),
            number,
            label: format!("Simulated panel {number}"),
            device: VirtualDevice::Proxy {
                source,
                device_type: DeviceType::CoverCalibrator,
            },
        });
    }
    let f = Fixture::from_config_with_workers(config, Some(workers.into())).await;
    assert!(f.hub.outputs().iter().all(|output| output.simulated));
    assert!(
        f.ok("GET", "/api/v1/covercalibrator/4/name", "")
            .await
            .as_str()
            .unwrap()
            .ends_with("(Simulation)")
    );
    for (slot, client) in [(4, 1), (17, 2)] {
        f.ok(
            "PUT",
            &format!("/api/v1/covercalibrator/{slot}/connected"),
            &format!("ClientID={client}&Connected=true"),
        )
        .await;
    }
    f.ok(
        "PUT",
        "/api/v1/covercalibrator/4/calibratoron",
        "ClientID=1&Brightness=17",
    )
    .await;
    assert_eq!(
        f.ok("GET", "/api/v1/covercalibrator/17/brightness", "ClientID=2")
            .await,
        17
    );
    f.ok("PUT", "/api/v1/covercalibrator/4/opencover", "ClientID=1")
        .await;
    eventually(async || {
        f.ok("GET", "/api/v1/covercalibrator/17/coverstate", "ClientID=2")
            .await
            == 3
    })
    .await;
    assert_eq!(
        f.ok(
            "GET",
            "/api/v1/covercalibrator/17/covermoving",
            "ClientID=2"
        )
        .await,
        false
    );
    f.ok(
        "PUT",
        "/api/v1/covercalibrator/4/connected",
        "ClientID=1&Connected=false",
    )
    .await;
    assert_eq!(f.hub.source_snapshot(source).unwrap().lease_count, 1);
    assert_eq!(
        f.ok("GET", "/api/v1/covercalibrator/17/brightness", "ClientID=2")
            .await,
        17
    );
    f.ok(
        "PUT",
        "/api/v1/covercalibrator/17/calibratoroff",
        "ClientID=2",
    )
    .await;
    f.finish().await;
}

#[tokio::test]
async fn panel_dedicated_simulation_keeps_http_operations_independent_and_uncertainty_fenced() {
    use regain_hub::config::{DeviceType, OutputConfig, SourceBackend, SourceConfig};
    let mut config = HubConfig::empty();
    let source = uuid::Uuid::new_v4();
    config.sources.push(SourceConfig {
        id: source,
        label: "Explicit panel simulation".into(),
        polling: regain_hub::parameters::PollPolicy::default(),
        backend: SourceBackend::Simulated {
            device_type: DeviceType::CoverCalibrator,
        },
    });
    for number in [4, 17] {
        config.outputs.push(OutputConfig {
            id: uuid::Uuid::new_v4(),
            number,
            label: format!("Panel {number}"),
            device: VirtualDevice::Proxy {
                source,
                device_type: DeviceType::CoverCalibrator,
            },
        });
    }
    let f = Fixture::from_config(config).await;
    let update = async |value| {
        f.hub
            .update_simulation(source, serde_json::from_value(value).unwrap())
            .await
            .unwrap()
    };
    update(json!({"fault":"stalledMotion","coverCalibrator":{"moveDurationSeconds":300,"lightDurationSeconds":0}})).await;
    assert!(f.hub.outputs().iter().all(|output| output.simulated));
    for (slot, client) in [(4, 1), (17, 2)] {
        f.ok(
            "PUT",
            &format!("/api/v1/covercalibrator/{slot}/connected"),
            &format!("ClientID={client}&Connected=true"),
        )
        .await;
    }
    f.ok("PUT", "/api/v1/covercalibrator/4/opencover", "ClientID=1")
        .await;
    f.ok(
        "PUT",
        "/api/v1/covercalibrator/4/calibratoron",
        "ClientID=1&Brightness=0",
    )
    .await;
    for member in ["covermoving", "calibratorchanging"] {
        assert_eq!(
            f.ok(
                "GET",
                &format!("/api/v1/covercalibrator/17/{member}"),
                "ClientID=2"
            )
            .await,
            true
        );
    }
    update(json!({"fault":"none"})).await;
    assert_eq!(
        f.ok(
            "GET",
            "/api/v1/covercalibrator/17/calibratorstate",
            "ClientID=2"
        )
        .await,
        3
    );
    assert_eq!(
        f.ok("GET", "/api/v1/covercalibrator/17/brightness", "ClientID=2")
            .await,
        0
    );
    assert_eq!(
        f.ok(
            "GET",
            "/api/v1/covercalibrator/17/covermoving",
            "ClientID=2"
        )
        .await,
        true
    );
    f.ok(
        "PUT",
        "/api/v1/covercalibrator/4/connected",
        "ClientID=1&Connected=false",
    )
    .await;
    eventually(async || f.hub.source_snapshot(source).unwrap().lease_count == 1).await;
    f.ok(
        "PUT",
        "/api/v1/covercalibrator/17/calibratoroff",
        "ClientID=2",
    )
    .await;
    assert_eq!(
        f.ok(
            "GET",
            "/api/v1/covercalibrator/17/covermoving",
            "ClientID=2"
        )
        .await,
        true
    );
    f.ok("PUT", "/api/v1/covercalibrator/17/haltcover", "ClientID=2")
        .await;
    assert_eq!(
        f.ok("GET", "/api/v1/covercalibrator/17/coverstate", "ClientID=2")
            .await,
        4
    );
    update(json!({"fault":"invalidMotion"})).await;
    assert_eq!(
        f.call(
            "GET",
            "/api/v1/covercalibrator/17/covermoving",
            "ClientID=2"
        )
        .await["ErrorNumber"],
        0x402
    );
    update(json!({"fault":"uncertainWrite"})).await;
    assert_eq!(
        f.call(
            "PUT",
            "/api/v1/covercalibrator/17/calibratoron",
            "ClientID=2&Brightness=17"
        )
        .await["ErrorNumber"],
        0x500
    );
    update(json!({"fault":"none"})).await;
    for member in ["calibratoroff", "haltcover", "closecover"] {
        assert_eq!(
            f.call(
                "PUT",
                &format!("/api/v1/covercalibrator/17/{member}"),
                "ClientID=2"
            )
            .await["ErrorNumber"],
            0x500
        );
    }
    let observed = f.hub.source_snapshot(source).unwrap();
    assert!(observed.write_uncertain);
    assert_eq!(
        observed
            .simulation
            .unwrap()
            .cover_calibrator
            .unwrap()
            .brightness,
        17
    );
    let hub = f.hub.clone();
    f.finish().await;
    let stopped = hub
        .source_snapshot(source)
        .unwrap()
        .simulation
        .unwrap()
        .cover_calibrator
        .unwrap();
    assert_eq!(stopped.brightness, 17);
    assert_eq!(stopped.calibrator_state, 3);
    assert_eq!(stopped.cover_state, 4);
}
