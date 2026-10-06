use super::*;
use std::sync::atomic::Ordering::SeqCst;

#[tokio::test]
async fn dedicated_rotator_simulation_publishes_shared_typed_http_without_workers() {
    use regain_hub::{
        config::{DeviceType, OutputConfig, SourceBackend, SourceConfig},
        simulated::{Fault, RotatorUpdate},
    };
    let mut config = HubConfig::empty();
    let source = uuid::Uuid::new_v4();
    config.sources.push(SourceConfig {
        id: source,
        label: "Explicit rotator simulation".into(),
        backend: SourceBackend::Simulated {
            device_type: DeviceType::Rotator,
        },
        polling: Default::default(),
    });
    for number in [4, 7] {
        config.outputs.push(OutputConfig {
            id: uuid::Uuid::new_v4(),
            number,
            label: format!("Simulator {number}"),
            device: VirtualDevice::Proxy {
                source,
                device_type: DeviceType::Rotator,
            },
        });
    }
    let f = Fixture::from_config(config).await;
    assert!(f.hub.outputs().iter().all(|output| output.simulated));
    assert_eq!(f.hub.source_snapshot(source).unwrap().lease_count, 0);
    for (number, client) in [(4, 1), (7, 2)] {
        f.ok(
            "PUT",
            &format!("/api/v1/rotator/{number}/connected"),
            &format!("ClientID={client}&Connected=true"),
        )
        .await;
    }
    f.ok("PUT", "/api/v1/rotator/4/sync", "ClientID=1&Position=42.5")
        .await;
    f.ok(
        "PUT",
        "/api/v1/rotator/7/reverse",
        "ClientID=2&Reverse=true",
    )
    .await;
    assert_eq!(
        f.ok("GET", "/api/v1/rotator/4/reverse", "ClientID=1").await,
        true
    );
    for (member, position, logical, mechanical) in [
        ("move", -721.5, 41.0, 358.5),
        ("moveabsolute", 50.0, 50.0, 7.5),
        ("movemechanical", 355.0, 37.5, 355.0),
    ] {
        f.ok(
            "PUT",
            &format!("/api/v1/rotator/4/{member}"),
            &format!("ClientID=1&Position={position}"),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(3), async {
            while f
                .ok("GET", "/api/v1/rotator/7/ismoving", "ClientID=2")
                .await
                == true
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            f.ok("GET", "/api/v1/rotator/7/position", "ClientID=2")
                .await
                .as_f64(),
            Some(logical)
        );
        assert_eq!(
            f.ok("GET", "/api/v1/rotator/7/mechanicalposition", "ClientID=2")
                .await
                .as_f64(),
            Some(mechanical)
        );
    }
    f.hub
        .update_simulation(
            source,
            SimulationUpdate {
                rotator: Some(RotatorUpdate {
                    step_size_available: Some(false),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        f.call("GET", "/api/v1/rotator/4/stepsize", "ClientID=1")
            .await["ErrorNumber"],
        1024
    );
    f.ok(
        "PUT",
        "/api/v1/rotator/4/connected",
        "ClientID=1&Connected=false",
    )
    .await;
    assert_eq!(
        f.ok("GET", "/api/v1/rotator/7/connected", "ClientID=2")
            .await,
        true
    );
    f.hub
        .update_simulation(
            source,
            SimulationUpdate {
                fault: Some(Fault::UncertainWrite),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        f.call("PUT", "/api/v1/rotator/7/move", "ClientID=2&Position=5")
            .await["ErrorNumber"],
        1280
    );
    f.hub
        .update_simulation(
            source,
            SimulationUpdate {
                fault: Some(Fault::None),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        f.call("PUT", "/api/v1/rotator/7/halt", "ClientID=2").await["ErrorNumber"],
        1280
    );
    assert!(f.hub.source_snapshot(source).unwrap().write_uncertain);
    f.finish().await;
}

#[tokio::test]
async fn rotator_local_slots_coexist_with_hub_outputs_without_opening_equipment() {
    let upstream = AccessoryUpstream::rotator(3).await;
    let f = Fixture::from_config(upstream.config(&[4])).await;
    assert_eq!(
        f.server
            .profiles
            .rotators
            .add(regain_alpaca::slots::Kind::Caa)
            .unwrap(),
        0
    );
    assert_eq!(
        f.ok("GET", "/api/v1/rotator/0/interfaceversion", "").await,
        3
    );
    assert_eq!(
        f.ok("GET", "/api/v1/rotator/0/connected", "ClientID=1")
            .await,
        false
    );
    assert_eq!(
        f.ok("GET", "/api/v1/rotator/4/interfaceversion", "").await,
        4
    );
    assert_eq!(
        f.ok("GET", "/api/v1/rotator/4/connected", "ClientID=1")
            .await,
        false
    );
    for (slot, script) in [(0, "/rotator.js"), (4, "/hub.mjs")] {
        let page = f
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/setup/v1/rotator/{slot}/setup"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(page.status(), StatusCode::OK);
        let html = String::from_utf8(
            page.into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        assert!(html.contains(script));
    }
    let missing = f
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/setup/v1/rotator/19/setup")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        request(&f.router, "GET", "/api/v1/rotator/4/Position", "")
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert!(upstream.writes.lock().unwrap().is_empty());
    assert_eq!(f.hub.active_connections(), 0);
    f.finish().await;
    upstream.finish().await;
}

#[tokio::test]
async fn rotator_native_workers_publish_verified_motion_and_reference_through_actual_http() {
    use regain_hub::config::{DeviceType, NativeDevice, OutputConfig, SourceBackend, SourceConfig};
    let Some(workers) = std::env::var_os("REGAIN_TEST_WORKERS") else {
        eprintln!(
            "Native rotator publication requires REGAIN_TEST_WORKERS pointing to built production workers"
        );
        return;
    };
    for (device, identity) in [
        (NativeDevice::Caa, "0102030405060708"),
        (NativeDevice::Falcon, "FALCON-SIMULATION"),
    ] {
        let mut config = HubConfig::empty();
        let source = uuid::Uuid::new_v4();
        config.sources.push(SourceConfig {
            id: source,
            label: "Explicit native rotator simulation".into(),
            backend: SourceBackend::Native {
                device,
                identity: identity.into(),
                filter_wheel: None,
            },
            polling: regain_hub::parameters::PollPolicy {
                request_timeout_seconds: 5.0,
                poll_seconds: 0.2,
                ..regain_hub::parameters::PollPolicy::default()
            },
        });
        for number in [4, 7] {
            config.outputs.push(OutputConfig {
                id: uuid::Uuid::new_v4(),
                number,
                label: format!("Explicit simulation {number}"),
                device: VirtualDevice::Proxy {
                    source,
                    device_type: DeviceType::Rotator,
                },
            });
        }
        let f = Fixture::from_config_with_workers(config, Some(workers.clone().into())).await;
        assert!(f.hub.outputs().iter().all(|item| item.simulated));
        assert_eq!(f.hub.source_snapshot(source).unwrap().lease_count, 0);
        for (slot, client) in [(4, 1), (7, 2)] {
            f.ok(
                "PUT",
                &format!("/api/v1/rotator/{slot}/connected"),
                &format!("ClientID={client}&Connected=true"),
            )
            .await;
        }
        assert_eq!(f.hub.source_snapshot(source).unwrap().lease_count, 2);
        let mechanical = f
            .ok("GET", "/api/v1/rotator/4/mechanicalposition", "ClientID=1")
            .await
            .as_f64()
            .unwrap();
        f.ok("PUT", "/api/v1/rotator/4/sync", "ClientID=1&Position=42.5")
            .await;
        assert_eq!(
            f.ok("GET", "/api/v1/rotator/7/position", "ClientID=2")
                .await,
            42.5
        );
        assert_eq!(
            f.ok("GET", "/api/v1/rotator/7/mechanicalposition", "ClientID=2")
                .await,
            mechanical
        );
        f.ok(
            "PUT",
            "/api/v1/rotator/7/moveabsolute",
            "ClientID=2&Position=43.5",
        )
        .await;
        tokio::time::timeout(Duration::from_secs(10), async {
            while f
                .ok("GET", "/api/v1/rotator/7/ismoving", "ClientID=2")
                .await
                == true
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let position = f
            .ok("GET", "/api/v1/rotator/7/position", "ClientID=2")
            .await
            .as_f64()
            .unwrap();
        assert!((position - 43.5).abs() < 0.05, "{device:?}: {position}");
        f.ok(
            "PUT",
            "/api/v1/rotator/4/connected",
            "ClientID=1&Connected=false",
        )
        .await;
        assert_eq!(f.hub.source_snapshot(source).unwrap().lease_count, 1);
        assert_eq!(
            f.ok("GET", "/api/v1/rotator/7/connected", "ClientID=2")
                .await,
            true
        );
        f.ok(
            "PUT",
            "/api/v1/rotator/7/connected",
            "ClientID=2&Connected=false",
        )
        .await;
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let state = f.hub.source_snapshot(source).unwrap();
                if state.lease_count == 0 && !state.transport_connected {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        f.ok(
            "PUT",
            "/api/v1/rotator/4/connected",
            "ClientID=3&Connected=true",
        )
        .await;
        let logical = f
            .ok("GET", "/api/v1/rotator/4/position", "ClientID=3")
            .await
            .as_f64()
            .unwrap();
        let physical = f
            .ok("GET", "/api/v1/rotator/4/mechanicalposition", "ClientID=3")
            .await
            .as_f64()
            .unwrap();
        assert!(
            ((logical - physical).rem_euclid(360.0) - (42.5 - mechanical).rem_euclid(360.0)).abs()
                < 0.05
        );
        f.finish().await;
    }
}

async fn await_connection(f: &Fixture, slot: u32, client: u32) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if f.ok(
                "GET",
                &format!("/api/v1/rotator/{slot}/connecting"),
                &format!("ClientID={client}"),
            )
            .await
                == false
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn rotator_publication_routes_dynamic_identity_all_commands_and_shared_ownership() {
    let upstream = AccessoryUpstream::rotator(3).await;
    let other = AccessoryUpstream::rotator(4).await;
    let mut config = upstream.config(&[4, 7]);
    let extra = other.config(&[12]);
    config.sources.extend(extra.sources);
    config.outputs.extend(extra.outputs);
    let f = Fixture::from_config(config).await;
    let catalog = f
        .ok(
            "GET",
            "/management/v1/configureddevices",
            "ClientTransactionID=91",
        )
        .await;
    assert_eq!(catalog.as_array().unwrap().len(), f.config.outputs.len());
    for (item, saved) in catalog.as_array().unwrap().iter().zip(&f.config.outputs) {
        assert_eq!(item["DeviceType"], "Rotator");
        assert_eq!(item["DeviceNumber"], saved.number);
        assert_eq!(item["UniqueID"], json!(saved.id));
    }
    assert_eq!(
        f.ok("GET", "/api/v1/rotator/4/interfaceversion", "").await,
        4
    );
    assert_eq!(
        f.call("GET", "/api/v1/rotator/4/position", "ClientID=1")
            .await["ErrorNumber"],
        0x407
    );
    for slot in [0, 5, 19] {
        assert_eq!(
            request(
                &f.router,
                "GET",
                &format!("/api/v1/rotator/{slot}/connected"),
                ""
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
    }
    let page = f
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/setup/v1/rotator/4/setup")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    let html = String::from_utf8(
        page.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(html.contains("src=\"/hub.mjs\""));
    assert_eq!(f.hub.active_connections(), 0);
    assert!(upstream.writes.lock().unwrap().is_empty());
    f.ok(
        "PUT",
        "/api/v1/rotator/4/connected",
        "ClientID=1&Connected=true",
    )
    .await;
    f.ok("PUT", "/api/v1/rotator/7/connect", "ClientID=2").await;
    await_connection(&f, 7, 2).await;
    assert!(upstream.connected.load(SeqCst));
    assert_eq!(
        f.hub
            .source_snapshot(upstream.source.id)
            .unwrap()
            .lease_count,
        2
    );
    for (member, expected) in [
        ("canreverse", json!(true)),
        ("reverse", json!(false)),
        ("ismoving", json!(false)),
        ("position", json!(20.0)),
        ("mechanicalposition", json!(350.0)),
        ("targetposition", json!(20.0)),
        ("stepsize", json!(0.02)),
    ] {
        assert_eq!(
            f.ok("GET", &format!("/api/v1/rotator/4/{member}"), "ClientID=1")
                .await,
            expected
        );
    }
    f.ok("PUT", "/api/v1/rotator/4/sync", "ClientID=1&Position=42.5")
        .await;
    f.ok(
        "PUT",
        "/api/v1/rotator/7/reverse",
        "ClientID=2&Reverse=true",
    )
    .await;
    assert_eq!(
        f.ok("GET", "/api/v1/rotator/4/reverse", "ClientID=1").await,
        true
    );
    assert_eq!(
        f.ok("GET", "/api/v1/rotator/4/position", "ClientID=1")
            .await,
        42.5
    );
    assert_eq!(
        f.ok("GET", "/api/v1/rotator/4/mechanicalposition", "ClientID=1")
            .await,
        350.0
    );
    f.ok(
        "PUT",
        "/api/v1/rotator/4/move",
        "ClientID=1&Position=-721.5",
    )
    .await;
    assert_eq!(
        f.ok("GET", "/api/v1/rotator/7/targetposition", "ClientID=2")
            .await,
        41.0
    );
    assert_eq!(
        f.call(
            "PUT",
            "/api/v1/rotator/7/moveabsolute",
            "ClientID=2&Position=12.5"
        )
        .await["ErrorNumber"],
        0x40b
    );
    f.ok("PUT", "/api/v1/rotator/7/halt", "ClientID=2").await;
    f.ok(
        "PUT",
        "/api/v1/rotator/7/movemechanical",
        "ClientID=2&Position=12.25",
    )
    .await;
    f.ok("PUT", "/api/v1/rotator/7/halt", "ClientID=2").await;
    f.ok(
        "PUT",
        "/api/v1/rotator/4/moveabsolute",
        "ClientID=1&Position=89.75",
    )
    .await;
    f.ok(
        "PUT",
        "/api/v1/rotator/4/connected",
        "ClientID=1&Connected=false",
    )
    .await;
    assert_eq!(
        f.ok("GET", "/api/v1/rotator/7/ismoving", "ClientID=2")
            .await,
        true
    );
    assert!(upstream.connected.load(SeqCst));
    assert_eq!(
        f.hub
            .source_snapshot(upstream.source.id)
            .unwrap()
            .lease_count,
        1
    );
    let writes = upstream.writes.lock().unwrap().clone();
    assert_eq!(
        writes.iter().filter(|(member, _)| member == "halt").count(),
        2
    );
    assert!(
        writes
            .iter()
            .any(|(member, args)| member == "move" && args["Position"] == "-721.5")
    );
    assert!(
        writes
            .iter()
            .any(|(member, args)| member == "movemechanical" && args["Position"] == "12.25")
    );
    f.ok("PUT", "/api/v1/rotator/12/connect", "ClientID=3")
        .await;
    await_connection(&f, 12, 3).await;
    assert_eq!(
        f.ok("GET", "/api/v1/rotator/12/position", "ClientID=3")
            .await,
        20.0
    );
    assert!(
        other
            .writes
            .lock()
            .unwrap()
            .iter()
            .any(|(member, _)| member == "connect")
    );
    f.ok("PUT", "/api/v1/rotator/7/disconnect", "ClientID=2")
        .await;
    await_connection(&f, 7, 2).await;
    // Client disconnection retires its output immediately. SourceLease::drop
    // schedules last-owner cleanup; Connecting=false does not await that task.
    tokio::time::timeout(Duration::from_secs(3), async {
        while upstream.connected.load(SeqCst) {
            assert!(other.connected.load(SeqCst));
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(!upstream.connected.load(SeqCst));
    assert!(other.connected.load(SeqCst));
    f.finish().await;
    upstream.finish().await;
    other.finish().await;
}

#[tokio::test]
async fn rotator_publication_rejects_invalid_values_and_preserves_optional_errors_and_cache() {
    let upstream = AccessoryUpstream::rotator(4).await;
    let f = Fixture::from_config(upstream.config(&[4])).await;
    f.ok(
        "PUT",
        "/api/v1/rotator/4/connected",
        "ClientID=1&Connected=true",
    )
    .await;
    for (member, value) in [
        ("moveabsolute", "-1"),
        ("moveabsolute", "360"),
        ("movemechanical", "360"),
        ("sync", "360"),
        ("move", "NaN"),
        ("move", "inf"),
        ("move", "3.5e38"),
        ("move", "bogus"),
    ] {
        assert_eq!(
            f.call(
                "PUT",
                &format!("/api/v1/rotator/4/{member}"),
                &format!("ClientID=1&Position={value}")
            )
            .await["ErrorNumber"],
            0x401
        );
    }
    assert_eq!(
        f.call("PUT", "/api/v1/rotator/4/reverse", "ClientID=1&Reverse=1")
            .await["ErrorNumber"],
        0x401
    );
    assert_eq!(
        f.call(
            "PUT",
            "/api/v1/rotator/4/move",
            "ClientID=1&Position=1&position=2"
        )
        .await["ErrorNumber"],
        0x401
    );
    assert_eq!(
        f.call("GET", "/api/v1/rotator/4/getswitch", "ClientID=1&Id=0")
            .await["ErrorNumber"],
        0x400
    );
    assert_eq!(upstream.writes.lock().unwrap().len(), 1);
    upstream.values.lock().unwrap().remove("stepsize");
    let optional = f
        .call("GET", "/api/v1/rotator/4/stepsize", "ClientID=1")
        .await;
    assert_eq!(optional["ErrorNumber"], 0x400);
    assert!(!optional.to_string().contains("private detail"));
    upstream
        .values
        .lock()
        .unwrap()
        .insert("position".into(), json!(360.0));
    upstream
        .values
        .lock()
        .unwrap()
        .insert("ismoving".into(), json!(1));
    assert_eq!(
        f.call("GET", "/api/v1/rotator/4/position", "ClientID=1")
            .await["ErrorNumber"],
        0x402
    );
    assert_eq!(
        f.call("PUT", "/api/v1/rotator/4/move", "ClientID=1&Position=1")
            .await["ErrorNumber"],
        0x402
    );
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let state = f
                .ok("GET", "/api/v1/rotator/4/devicestate", "ClientID=1")
                .await;
            if state
                .as_array()
                .unwrap()
                .iter()
                .all(|item| item["Name"] != "Position" && item["Name"] != "IsMoving")
                && state
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|item| item["Name"] == "MechanicalPosition")
            {
                assert!(
                    state
                        .as_array()
                        .unwrap()
                        .iter()
                        .all(|item| item["Name"] == "MechanicalPosition")
                );
                assert_eq!(state[0]["Value"], 350.0);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    f.finish().await;
    upstream.finish().await;
}

#[tokio::test]
async fn rotator_lost_move_reply_keeps_shared_uncertainty_and_never_replays_or_halts() {
    let upstream = AccessoryUpstream::rotator(4).await;
    let f = Fixture::from_config(upstream.config(&[4, 7])).await;
    for (slot, client) in [(4, 1), (7, 2)] {
        f.ok(
            "PUT",
            &format!("/api/v1/rotator/{slot}/connected"),
            &format!("ClientID={client}&Connected=true"),
        )
        .await;
    }
    upstream.lose_move_reply.store(true, SeqCst);
    assert_eq!(
        f.call(
            "PUT",
            "/api/v1/rotator/4/moveabsolute",
            "ClientID=1&Position=25.5"
        )
        .await["ErrorNumber"],
        0x500
    );
    assert!(
        f.hub
            .source_snapshot(upstream.source.id)
            .unwrap()
            .write_uncertain
    );
    assert_eq!(
        f.ok("GET", "/api/v1/rotator/4/connected", "ClientID=1")
            .await,
        false
    );
    assert_eq!(
        f.call(
            "PUT",
            "/api/v1/rotator/7/moveabsolute",
            "ClientID=2&Position=33"
        )
        .await["ErrorNumber"],
        0x500
    );
    f.ok(
        "PUT",
        "/api/v1/rotator/4/connected",
        "ClientID=1&Connected=false",
    )
    .await;
    assert!(
        f.hub
            .source_snapshot(upstream.source.id)
            .unwrap()
            .write_uncertain
    );
    let writes = upstream.writes.lock().unwrap().clone();
    assert_eq!(
        writes
            .iter()
            .filter(|(member, _)| member == "moveabsolute")
            .count(),
        1
    );
    assert!(!writes.iter().any(|(member, _)| member == "halt"));
    f.finish().await;
    upstream.finish().await;
}

#[tokio::test]
async fn rotator_publication_rejects_slot_collisions_before_native_or_remote_connection() {
    let upstream = AccessoryUpstream::rotator(3).await;
    let f = Fixture::from_config(upstream.config(&[0])).await;
    assert_eq!(
        f.server
            .profiles
            .rotators
            .add(regain_alpaca::slots::Kind::Caa)
            .unwrap(),
        0
    );
    assert_eq!(
        f.call("GET", "/management/v1/configureddevices", "").await["ErrorNumber"],
        0x401
    );
    for member in ["connected", "position"] {
        let reply = f
            .call(
                "GET",
                &format!("/api/v1/rotator/0/{member}"),
                "ClientID=1&ClientTransactionID=92",
            )
            .await;
        assert_eq!(reply["ErrorNumber"], 0x401);
        assert_eq!(reply["ClientTransactionID"], 92);
    }
    for (member, data) in [
        ("connected", "Connected=true"),
        ("moveabsolute", "Position=20"),
    ] {
        let reply = f
            .call(
                "PUT",
                &format!("/api/v1/rotator/0/{member}"),
                &format!("ClientID=1&ClientTransactionID=93&{data}"),
            )
            .await;
        assert_eq!(reply["ErrorNumber"], 0x401);
        assert_eq!(reply["ClientTransactionID"], 93);
    }
    let page = f
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/setup/v1/rotator/0/setup")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(page.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(upstream.writes.lock().unwrap().is_empty());
    assert_eq!(f.hub.active_connections(), 0);
    f.finish().await;
    upstream.finish().await;
}

#[tokio::test]
async fn rotator_missing_reversal_fails_sync_or_async_connection_without_write_or_replay() {
    for missing in ["canreverse", "reverse"] {
        for asynchronous in [false, true] {
            let upstream = AccessoryUpstream::rotator(4).await;
            if missing == "canreverse" {
                upstream
                    .values
                    .lock()
                    .unwrap()
                    .insert("canreverse".into(), json!(false));
            } else {
                upstream.values.lock().unwrap().remove("reverse");
            }
            let f = Fixture::from_config(upstream.config(&[4])).await;
            if asynchronous {
                f.ok("PUT", "/api/v1/rotator/4/connect", "ClientID=1").await;
                tokio::time::timeout(Duration::from_secs(3), async {
                    loop {
                        let reply = f
                            .call("GET", "/api/v1/rotator/4/connecting", "ClientID=1")
                            .await;
                        if reply["ErrorNumber"] != 0 {
                            assert_eq!(reply["ErrorNumber"], 0x402);
                            assert!(
                                reply["ErrorMessage"]
                                    .as_str()
                                    .unwrap()
                                    .contains("supports and reports reversal")
                            );
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await
                .unwrap();
            } else {
                let reply = f
                    .call(
                        "PUT",
                        "/api/v1/rotator/4/connected",
                        "ClientID=1&Connected=true",
                    )
                    .await;
                assert_eq!(reply["ErrorNumber"], 0x402);
                assert!(
                    reply["ErrorMessage"]
                        .as_str()
                        .unwrap()
                        .contains("supports and reports reversal")
                );
            }
            assert_eq!(
                f.ok("GET", "/api/v1/rotator/4/connected", "ClientID=1")
                    .await,
                false
            );
            assert!(
                !upstream
                    .writes
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(member, _)| ["move", "sync", "reverse"].contains(&member.as_str()))
            );
            f.ok(
                "PUT",
                "/api/v1/rotator/4/connected",
                "ClientID=1&Connected=false",
            )
            .await;
            tokio::time::timeout(Duration::from_secs(3), async {
                while f
                    .hub
                    .source_snapshot(upstream.source.id)
                    .unwrap()
                    .lease_count
                    != 0
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(f.hub.active_connections(), 0);
            f.finish().await;
            upstream.finish().await;
        }
    }
}
