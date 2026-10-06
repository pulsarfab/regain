use super::*;
use std::sync::atomic::Ordering::SeqCst;

fn moves(upstream: &AccessoryUpstream) -> Vec<AccessoryWrite> {
    upstream
        .writes
        .lock()
        .unwrap()
        .iter()
        .filter(|(member, _)| member == "position")
        .cloned()
        .collect()
}
async fn page(f: &Fixture, slot: u32) -> (StatusCode, String) {
    let response = f
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/setup/v1/filterwheel/{slot}/setup"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    (
        status,
        String::from_utf8(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap(),
    )
}

#[tokio::test]
async fn wheel_publication_preserves_dynamic_identity_metadata_shared_ownership_and_local_routes() {
    for version in [2, 3] {
        let upstream = AccessoryUpstream::filterwheel(version).await;
        let other = AccessoryUpstream::filterwheel(version).await;
        let mut config = upstream.config(&[4, 17]);
        let extra = other.config(&[12]);
        config.sources.extend(extra.sources);
        config.outputs.extend(extra.outputs);
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
            assert_eq!(device["DeviceType"], "FilterWheel");
            assert_eq!(device["DeviceNumber"], output.number);
            assert_eq!(device["DeviceName"], output.label);
        }
        assert!(upstream.writes.lock().unwrap().is_empty());
        assert!(other.writes.lock().unwrap().is_empty());
        assert_eq!(
            f.hub
                .source_snapshot(upstream.source.id)
                .unwrap()
                .lease_count,
            0
        );
        assert_eq!(
            f.ok("GET", "/api/v1/filterwheel/4/interfaceversion", "")
                .await,
            3
        );
        assert_eq!(
            f.call("GET", "/api/v1/filterwheel/4/names", "ClientID=1")
                .await["ErrorNumber"],
            0x407
        );
        let (status, html) = page(&f, 4).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("hub.mjs"));
        assert_eq!(page(&f, 5).await.0, StatusCode::NOT_FOUND);
        assert_eq!(
            request(&f.router, "GET", "/api/v1/filterwheel/5/connected", "")
                .await
                .0,
            StatusCode::NOT_FOUND
        );
        f.server
            .filterwheel
            .configure(json!({"serial":"0102030405060708","label":"Private local wheel"}))
            .await
            .unwrap();
        assert_eq!(
            f.ok("GET", "/api/v1/filterwheel/0/connected", "ClientID=99")
                .await,
            false
        );
        assert_eq!(
            f.ok("GET", "/api/v1/filterwheel/0/name", "").await,
            "Private local wheel"
        );
        assert!(page(&f, 0).await.1.contains("accessory.js"));
        assert_eq!(
            f.ok("GET", "/management/v1/configureddevices", "")
                .await
                .as_array()
                .unwrap()
                .len(),
            4
        );
        f.ok(
            "PUT",
            "/api/v1/filterwheel/4/connected",
            "ClientID=1&Connected=true",
        )
        .await;
        f.ok("PUT", "/api/v1/filterwheel/17/connect", "ClientID=2")
            .await;
        eventually(async || {
            f.ok("GET", "/api/v1/filterwheel/17/connecting", "ClientID=2")
                .await
                == false
        })
        .await;
        assert_eq!(
            f.hub
                .source_snapshot(upstream.source.id)
                .unwrap()
                .lease_count,
            2
        );
        assert_eq!(
            f.ok("GET", "/api/v1/filterwheel/4/names", "ClientID=1")
                .await,
            json!(["L", "Hα", ""])
        );
        assert_eq!(
            f.ok("GET", "/api/v1/filterwheel/17/focusoffsets", "ClientID=2")
                .await,
            json!([-12, 0, 17])
        );
        for value in ["-1", "3", "2147483648", "-2147483649", "1.5", "bogus"] {
            assert_eq!(
                f.call(
                    "PUT",
                    "/api/v1/filterwheel/4/position",
                    &format!("ClientID=1&Position={value}")
                )
                .await["ErrorNumber"],
                0x401
            );
        }
        assert_eq!(
            f.call(
                "PUT",
                "/api/v1/filterwheel/4/position",
                "ClientID=1&Position=1&position=2"
            )
            .await["ErrorNumber"],
            0x401
        );
        for member in ["halt", "calibrate", "move", "commandblind"] {
            assert_eq!(
                f.call(
                    "PUT",
                    &format!("/api/v1/filterwheel/4/{member}"),
                    "ClientID=1&Position=1"
                )
                .await["ErrorNumber"],
                0x400
            );
        }
        assert!(moves(&upstream).is_empty());
        f.ok(
            "PUT",
            "/api/v1/filterwheel/4/position",
            "ClientID=1&Position=2",
        )
        .await;
        assert_eq!(
            f.ok("GET", "/api/v1/filterwheel/17/position", "ClientID=2")
                .await,
            -1
        );
        assert_eq!(
            f.call(
                "PUT",
                "/api/v1/filterwheel/17/position",
                "ClientID=2&Position=1"
            )
            .await["ErrorNumber"],
            0x40b
        );
        eventually(async || {
            f.ok("GET", "/api/v1/filterwheel/17/devicestate", "ClientID=2")
                .await
                == json!([{"Name":"Position","Value":-1}])
        })
        .await;
        assert_eq!(moves(&upstream).len(), 1);
        assert_eq!(moves(&upstream)[0].1["Position"], "2");
        upstream
            .values
            .lock()
            .unwrap()
            .insert("position".into(), json!(2));
        f.ok(
            "PUT",
            "/api/v1/filterwheel/4/connected",
            "ClientID=1&Connected=false",
        )
        .await;
        assert!(upstream.connected.load(SeqCst));
        assert_eq!(
            f.ok("GET", "/api/v1/filterwheel/17/position", "ClientID=2")
                .await,
            2
        );
        f.ok(
            "PUT",
            "/api/v1/filterwheel/12/connected",
            "ClientID=3&Connected=true",
        )
        .await;
        assert_eq!(
            f.ok("GET", "/api/v1/filterwheel/12/position", "ClientID=3")
                .await,
            0
        );
        f.ok("PUT", "/api/v1/filterwheel/17/disconnect", "ClientID=2")
            .await;
        eventually(async || !upstream.connected.load(SeqCst)).await;
        assert!(other.connected.load(SeqCst));
        assert_eq!(
            upstream
                .writes
                .lock()
                .unwrap()
                .iter()
                .filter(|(member, _)| member == if version == 3 { "connect" } else { "connected" })
                .count(),
            if version == 3 { 1 } else { 2 }
        );
        f.finish().await;
        upstream.finish().await;
        other.finish().await;
    }
}

#[tokio::test]
async fn wheel_publication_rejects_malformed_live_metadata_without_motion_or_fabrication() {
    let upstream = AccessoryUpstream::filterwheel(3).await;
    let f = Fixture::from_config(upstream.config(&[4])).await;
    f.ok(
        "PUT",
        "/api/v1/filterwheel/4/connected",
        "ClientID=1&Connected=true",
    )
    .await;
    let original = upstream.values.lock().unwrap().clone();
    for (key, value) in [
        ("names", json!([])),
        ("names", json!(["L"])),
        ("focusoffsets", json!([-12, 1, 17])),
        ("focusoffsets", json!([0, 1.5, 17])),
        ("focusoffsets", json!([0, 2147483648_i64, 17])),
        ("position", json!(3)),
        ("position", json!(-2)),
    ] {
        *upstream.values.lock().unwrap() = original.clone();
        upstream.values.lock().unwrap().insert(key.into(), value);
        assert_eq!(
            f.call("GET", "/api/v1/filterwheel/4/position", "ClientID=1")
                .await["ErrorNumber"],
            0x402
        );
        assert_eq!(
            f.call(
                "PUT",
                "/api/v1/filterwheel/4/position",
                "ClientID=1&Position=2"
            )
            .await["ErrorNumber"],
            0x402
        );
        assert!(moves(&upstream).is_empty());
    }
    *upstream.values.lock().unwrap() = original;
    assert_eq!(
        f.ok("GET", "/api/v1/filterwheel/4/position", "ClientID=1")
            .await,
        0
    );
    upstream.values.lock().unwrap().remove("names");
    let missing = f
        .call("GET", "/api/v1/filterwheel/4/names", "ClientID=1")
        .await;
    assert_eq!(missing["ErrorNumber"], 0x400);
    assert!(!missing.to_string().contains("private detail"));
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
async fn wheel_publication_rejects_slot_zero_collision_without_opening_equipment() {
    let upstream = AccessoryUpstream::filterwheel(3).await;
    let f = Fixture::from_config(upstream.config(&[0])).await;
    assert_eq!(
        f.ok("GET", "/management/v1/configureddevices", "")
            .await
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(page(&f, 0).await.1.contains("hub.mjs"));
    f.server
        .filterwheel
        .configure(json!({"serial":"0102030405060708"}))
        .await
        .unwrap();
    assert_eq!(
        f.call("GET", "/management/v1/configureddevices", "").await["ErrorNumber"],
        0x401
    );
    for member in ["connected", "position"] {
        let reply = f
            .call(
                "GET",
                &format!("/api/v1/filterwheel/0/{member}"),
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
    f.server.filterwheel.configure(json!({})).await.unwrap();
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
async fn wheel_lost_position_reply_fences_siblings_without_replay_or_extra_commands() {
    for version in [2, 3] {
        let upstream = AccessoryUpstream::filterwheel(version).await;
        let f = Fixture::from_config(upstream.config(&[4, 17])).await;
        for (slot, client) in [(4, 1), (17, 2)] {
            f.ok(
                "PUT",
                &format!("/api/v1/filterwheel/{slot}/connected"),
                &format!("ClientID={client}&Connected=true"),
            )
            .await;
        }
        upstream.lose_move_reply.store(true, SeqCst);
        let reply = f
            .call(
                "PUT",
                "/api/v1/filterwheel/4/position",
                "ClientID=1&Position=2",
            )
            .await;
        assert_eq!(reply["ErrorNumber"], 0x500);
        assert!(
            reply["ErrorMessage"]
                .as_str()
                .unwrap()
                .contains("uncertain")
        );
        assert_eq!(moves(&upstream).len(), 1);
        let blocked = f
            .call(
                "PUT",
                "/api/v1/filterwheel/17/position",
                "ClientID=2&Position=1",
            )
            .await;
        assert_eq!(blocked["ErrorNumber"], 0x500);
        assert!(
            blocked["ErrorMessage"]
                .as_str()
                .unwrap()
                .contains("uncertain")
        );
        assert_eq!(
            f.ok("GET", "/api/v1/filterwheel/17/connected", "ClientID=2")
                .await,
            false
        );
        assert!(
            f.hub
                .source_snapshot(upstream.source.id)
                .unwrap()
                .write_uncertain
        );
        assert_eq!(moves(&upstream).len(), 1);
        assert!(upstream.writes.lock().unwrap().iter().all(|(member, _)| {
            ["connected", "connect", "disconnect", "position"].contains(&member.as_str())
        }));
        f.finish().await;
        upstream.finish().await;
    }
}

#[tokio::test]
async fn wheel_native_worker_publishes_preserved_arrays_and_shared_position_through_http() {
    use regain_hub::{
        config::{DeviceType, NativeDevice, OutputConfig, SourceBackend, SourceConfig},
        filterwheel::NativeFilterWheelMetadata,
    };
    let Some(workers) = std::env::var_os("REGAIN_TEST_WORKERS") else {
        eprintln!(
            "Native wheel publication requires REGAIN_TEST_WORKERS pointing to built production workers"
        );
        return;
    };
    let mut config = HubConfig::empty();
    let source = uuid::Uuid::new_v4();
    let names = vec!["L", "R", "G", "B", "Hα", "", "SII"];
    let offsets = vec![0, -12, 17, 30, 45, 52, 67];
    config.sources.push(SourceConfig {
        id: source,
        label: "Explicit EFW simulation".into(),
        polling: regain_hub::parameters::PollPolicy {
            poll_seconds: 0.2,
            request_timeout_seconds: 5.0,
            ..Default::default()
        },
        backend: SourceBackend::Native {
            device: NativeDevice::Efw,
            identity: "0102030405060708".into(),
            filter_wheel: Some(NativeFilterWheelMetadata {
                names: names.iter().map(|name| (*name).to_string()).collect(),
                focus_offsets: offsets.clone(),
            }),
        },
    });
    for number in [4, 17] {
        config.outputs.push(OutputConfig {
            id: uuid::Uuid::new_v4(),
            number,
            label: format!("Simulated wheel {number}"),
            device: VirtualDevice::Proxy {
                source,
                device_type: DeviceType::FilterWheel,
            },
        });
    }
    let f = Fixture::from_config_with_workers(config, Some(workers.into())).await;
    assert!(f.hub.outputs().iter().all(|output| output.simulated));
    assert!(
        f.ok("GET", "/api/v1/filterwheel/4/name", "")
            .await
            .as_str()
            .unwrap()
            .ends_with("(Simulation)")
    );
    for (slot, client) in [(4, 1), (17, 2)] {
        f.ok(
            "PUT",
            &format!("/api/v1/filterwheel/{slot}/connected"),
            &format!("ClientID={client}&Connected=true"),
        )
        .await;
    }
    assert_eq!(
        f.ok("GET", "/api/v1/filterwheel/4/names", "ClientID=1")
            .await,
        json!(names)
    );
    assert_eq!(
        f.ok("GET", "/api/v1/filterwheel/17/focusoffsets", "ClientID=2")
            .await,
        json!(offsets)
    );
    f.ok(
        "PUT",
        "/api/v1/filterwheel/4/position",
        "ClientID=1&Position=6",
    )
    .await;
    assert_eq!(
        f.ok("GET", "/api/v1/filterwheel/17/position", "ClientID=2")
            .await,
        6
    );
    f.ok(
        "PUT",
        "/api/v1/filterwheel/4/connected",
        "ClientID=1&Connected=false",
    )
    .await;
    assert_eq!(f.hub.source_snapshot(source).unwrap().lease_count, 1);
    assert_eq!(
        f.ok("GET", "/api/v1/filterwheel/17/position", "ClientID=2")
            .await,
        6
    );
    f.finish().await;
}
