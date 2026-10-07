use super::*;

fn configuration() -> HubConfig {
    let mut config = HubConfig::empty();
    let source = uuid::Uuid::new_v4();
    config.sources.push(
        serde_json::from_value(json!({"id":source,"label":"Explicit camera fixture",
        "backend":{"kind":"simulated","deviceType":"camera"},"polling":{"pollSeconds":0.1}}))
        .unwrap(),
    );
    for number in [4, 17] {
        config.outputs.push(serde_json::from_value(json!({"id":uuid::Uuid::new_v4(),"number":number,
            "label":"Shared camera fixture","device":{"kind":"proxy","source":source,"deviceType":"camera"}})).unwrap());
    }
    config
}
async fn ready(f: &Fixture, number: u32, client: u32) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if f.ok(
                "GET",
                &format!("/api/v1/camera/{number}/imageready"),
                &format!("ClientID={client}"),
            )
            .await
                == true
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
async fn image(f: &Fixture, number: u32, client: u32, binary: bool) -> axum::response::Response {
    f.router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/v1/camera/{number}/imagearray?ClientID={client}&ClientTransactionID=987"
                ))
                .header(
                    "Accept",
                    if binary {
                        "application/imagebytes"
                    } else {
                        "application/json"
                    },
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn camera_creation_through_shared_setup_is_inert_persistent_and_publishes_independent_outputs()
 {
    let f = Fixture::from_config(HubConfig::empty()).await;
    let invoke = async |command| {
        setup(
            &f.router,
            command,
            "application/json",
            "http://127.0.0.1:11111",
        )
        .await
    };
    let (status, description) = invoke(json!({"op":"describeConfig"})).await;
    assert_eq!(status, StatusCode::OK);
    let capabilities = description["result"]["capabilities"].as_array().unwrap();
    for capability in ["cameraOutputs", "cameraSimulation"] {
        assert!(capabilities.iter().any(|value| value == capability));
    }
    // This embedding has no native camera runtime or installed COM workers.
    for capability in [
        "nativeCameraSources",
        "cameraComSources",
        "broaderProxyOutputs",
    ] {
        assert!(!capabilities.iter().any(|value| value == capability));
    }
    let (_, saved) = invoke(json!({"op":"getConfig"})).await;
    let mut candidate = saved["result"].clone();
    let revision = candidate["revision"].clone();
    let created = configuration();
    candidate["sources"] = serde_json::to_value(&created.sources).unwrap();
    candidate["outputs"] = serde_json::to_value(&created.outputs).unwrap();
    let source = created.sources[0].id;
    let (_, valid) = invoke(json!({"op":"validateConfig","candidate":candidate})).await;
    assert_eq!(valid["result"]["valid"], true, "{valid}");
    assert!(
        f.ok("GET", "/management/v1/configureddevices", "")
            .await
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        invoke(json!({"op":"sourceStatus","source":source})).await.0,
        StatusCode::BAD_REQUEST
    );
    let mut invalid = candidate.clone();
    invalid["outputs"][0]["device"]["deviceType"] = json!("switch");
    let (_, result) = invoke(json!({"op":"validateConfig","candidate":invalid})).await;
    assert_eq!(result["result"]["valid"], false);
    let (status, applied) =
        invoke(json!({"op":"applyConfig","expectedRevision":revision,"candidate":candidate})).await;
    assert_eq!(status, StatusCode::OK, "{applied}");
    assert_eq!(applied["result"]["ready"], true);
    let (_, state) = invoke(json!({"op":"sourceStatus","source":source})).await;
    assert_eq!(state["result"]["leaseCount"], 0);
    assert_eq!(state["result"]["transportConnected"], false);
    let (_, reloaded) = invoke(json!({"op":"getConfig"})).await;
    let devices = f.ok("GET", "/management/v1/configureddevices", "").await;
    for (index, output) in created.outputs.iter().enumerate() {
        assert_eq!(reloaded["result"]["outputs"][index]["id"], json!(output.id));
        assert_eq!(
            reloaded["result"]["outputs"][index]["number"],
            output.number
        );
        assert_eq!(devices[index]["UniqueID"], json!(output.id));
        assert_eq!(devices[index]["DeviceNumber"], output.number);
    }
    let persisted: HubConfig =
        serde_json::from_slice(&std::fs::read(f._dir.path().join("hub.json")).unwrap()).unwrap();
    assert_eq!(persisted.outputs, created.outputs);
    for (number, client) in [(4, 1), (17, 2)] {
        f.ok(
            "PUT",
            &format!("/api/v1/camera/{number}/connected"),
            &format!("ClientID={client}&Connected=true"),
        )
        .await;
    }
    for (member, value) in [("numx", "NumX=96"), ("numy", "NumY=64")] {
        f.ok(
            "PUT",
            &format!("/api/v1/camera/4/{member}"),
            &format!("ClientID=1&{value}"),
        )
        .await;
    }
    let (status, blocked) = invoke(json!({"op":"applyConfig","expectedRevision":reloaded["result"]["revision"],"candidate":reloaded["result"]})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(blocked["error"]["code"], "connected");
    f.ok(
        "PUT",
        "/api/v1/camera/4/startexposure",
        "ClientID=1&Duration=0.01&Light=true",
    )
    .await;
    ready(&f, 4, 1).await;
    let retained = image(&f, 17, 2, true).await;
    assert_eq!(retained.status(), StatusCode::OK);
    f.ok(
        "PUT",
        "/api/v1/camera/4/connected",
        "ClientID=1&Connected=false",
    )
    .await;
    assert_eq!(
        f.ok("GET", "/api/v1/camera/17/connected", "ClientID=2")
            .await,
        true
    );
    let bytes = retained.into_body().collect().await.unwrap().to_bytes();
    let decoded = regain_hub::camera::image::read_imagebytes(
        &mut bytes.as_ref(),
        &regain_hub::camera::image::ImageBudget::new(64 * 1024).unwrap(),
        987,
    )
    .await
    .unwrap();
    assert_eq!(decoded.image.bytes().len(), 96 * 64 * 2);
    f.ok(
        "PUT",
        "/api/v1/camera/17/connected",
        "ClientID=2&Connected=false",
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (_, state) = invoke(json!({"op":"sourceStatus","source":source})).await;
            if state["result"]["leaseCount"] == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    f.finish().await;
}

#[tokio::test]
async fn shared_camera_publication_routes_identity_settings_guiding_and_both_image_formats() {
    let f = Fixture::from_config(configuration()).await;
    let devices = f.ok("GET", "/management/v1/configureddevices", "").await;
    assert_eq!(devices.as_array().unwrap().len(), 2);
    for (index, number) in [4, 17].into_iter().enumerate() {
        assert_eq!(devices[index]["DeviceType"], "Camera");
        assert_eq!(devices[index]["DeviceNumber"], number);
        assert_eq!(
            devices[index]["UniqueID"],
            json!(f.config.outputs[index].id)
        );
        assert_eq!(
            f.ok(
                "GET",
                &format!("/api/v1/camera/{number}/interfaceversion"),
                ""
            )
            .await,
            4
        );
        assert!(
            f.ok("GET", &format!("/api/v1/camera/{number}/name"), "")
                .await
                .as_str()
                .unwrap()
                .ends_with("(Simulation)")
        );
        let page = f
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/setup/v1/camera/{number}/setup"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(page.status(), StatusCode::OK);
        assert!(
            std::str::from_utf8(&page.into_body().collect().await.unwrap().to_bytes())
                .unwrap()
                .contains("hub.mjs")
        );
    }
    assert_eq!(
        request(&f.router, "GET", "/api/v1/camera/9/name", "")
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        f.call("GET", "/api/v1/camera/4/cameraxsize", "ClientID=1")
            .await["ErrorNumber"],
        0x407
    );
    f.hub
        .update_simulation(
            f.config.sources[0].id,
            serde_json::from_value(
                json!({"camera":{"canPulseGuide":true,"readoutDurationSeconds":0.01}}),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    for (number, client) in [(4, 1), (17, 2)] {
        f.ok(
            "PUT",
            &format!("/api/v1/camera/{number}/connected"),
            &format!("ClientID={client}&Connected=true"),
        )
        .await;
    }
    for (member, parameter) in [
        ("numx", "NumX=3"),
        ("numy", "NumY=2"),
        ("cooleron", "CoolerOn=true"),
        ("setccdtemperature", "SetCCDTemperature=-12.5"),
    ] {
        f.ok(
            "PUT",
            &format!("/api/v1/camera/4/{member}"),
            &format!("ClientID=1&{parameter}"),
        )
        .await;
    }
    assert_eq!(f.ok("GET", "/api/v1/camera/17/numx", "ClientID=2").await, 3);
    assert_eq!(
        f.ok("GET", "/api/v1/camera/17/cooleron", "ClientID=2")
            .await,
        true
    );
    assert_eq!(
        f.ok("GET", "/api/v1/camera/17/setccdtemperature", "ClientID=2")
            .await,
        -12.5
    );
    for (member, args) in [
        ("numx", "NumX=1.5"),
        ("numx", "NumX=0"),
        ("gain", "Gain=2147483648"),
        ("pulseguide", "Direction=4&Duration=1"),
        ("pulseguide", "Direction=1&Duration=-1"),
        ("pulseguide", "Direction=1&Duration=1.5"),
        ("startexposure", "Duration=NaN&Light=true"),
    ] {
        assert_eq!(
            f.call(
                "PUT",
                &format!("/api/v1/camera/4/{member}"),
                &format!("ClientID=1&{args}")
            )
            .await["ErrorNumber"],
            0x401
        );
    }
    f.ok(
        "PUT",
        "/api/v1/camera/4/pulseguide",
        "ClientID=1&Direction=0&Duration=1000",
    )
    .await;
    assert_eq!(
        f.ok("GET", "/api/v1/camera/17/ispulseguiding", "ClientID=2")
            .await,
        true
    );
    assert_eq!(
        f.call(
            "PUT",
            "/api/v1/camera/17/startexposure",
            "ClientID=2&Duration=0.01&Light=true"
        )
        .await["ErrorNumber"],
        0x40b
    );
    f.ok(
        "PUT",
        "/api/v1/camera/4/startexposure",
        "ClientID=1&Duration=0.01&Light=true",
    )
    .await;
    ready(&f, 4, 1).await;
    ready(&f, 17, 2).await;
    let json_response = image(&f, 17, 2, false).await;
    assert_eq!(json_response.headers()["cache-control"], "no-store");
    let json_response: Value = serde_json::from_slice(
        &json_response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes(),
    )
    .unwrap();
    assert_eq!(json_response["ClientTransactionID"], 987);
    assert_eq!(json_response["Rank"], 2);
    assert_eq!(json_response["Type"], 2);
    assert_eq!(json_response["Value"].as_array().unwrap().len(), 3);
    assert_eq!(json_response["Value"][0].as_array().unwrap().len(), 2);
    let binary_response = image(&f, 4, 1, true).await;
    assert_eq!(
        binary_response.headers()["content-type"],
        "application/imagebytes"
    );
    let bytes = binary_response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    let decoded = regain_hub::camera::image::read_imagebytes(
        &mut bytes.as_ref(),
        &regain_hub::camera::image::ImageBudget::new(1024).unwrap(),
        987,
    )
    .await
    .unwrap();
    for x in 0..3 {
        for y in 0..2 {
            let start = (x * 2 + y) * 2;
            assert_eq!(
                json_response["Value"][x][y],
                u16::from_le_bytes(decoded.image.bytes()[start..start + 2].try_into().unwrap())
            );
        }
    }
    // The reader pins immutable completed pixels across another capture.
    let pinned = image(&f, 4, 1, true).await;
    tokio::time::timeout(Duration::from_secs(3), async {
        while f
            .hub
            .camera_acquisition_status(f.config.sources[0].id)
            .unwrap()
            .guiding
            .is_some()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    f.ok(
        "PUT",
        "/api/v1/camera/4/readoutmode",
        "ClientID=1&ReadoutMode=1",
    )
    .await;
    f.ok(
        "PUT",
        "/api/v1/camera/4/startexposure",
        "ClientID=1&Duration=0.01&Light=true",
    )
    .await;
    ready(&f, 4, 1).await;
    let rgb = f
        .ok("GET", "/api/v1/camera/4/imagearrayvariant", "ClientID=1")
        .await;
    assert_eq!(rgb[0][0].as_array().unwrap().len(), 3);
    let pinned = pinned.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&pinned[..12], &bytes[..12]);
    assert_eq!(&pinned[16..], &bytes[16..]); // Server transaction differs per response.
    f.finish().await;
}

#[tokio::test]
async fn camera_binary_errors_disconnect_and_reader_admission_preserve_other_clients() {
    let f = Fixture::from_config(configuration()).await;
    let disconnected = image(&f, 4, 1, true)
        .await
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    assert_eq!(
        u32::from_le_bytes(disconnected[4..8].try_into().unwrap()),
        0x407
    );
    assert_eq!(
        u32::from_le_bytes(disconnected[8..12].try_into().unwrap()),
        987
    );
    for (number, client) in [(4, 1), (17, 2)] {
        f.ok(
            "PUT",
            &format!("/api/v1/camera/{number}/connected"),
            &format!("ClientID={client}&Connected=true"),
        )
        .await;
    }
    let not_ready = image(&f, 4, 1, true)
        .await
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    assert_eq!(
        u32::from_le_bytes(not_ready[4..8].try_into().unwrap()),
        0x402
    );
    f.ok(
        "PUT",
        "/api/v1/camera/4/startexposure",
        "ClientID=1&Duration=0.01&Light=true",
    )
    .await;
    ready(&f, 4, 1).await;
    let mut readers = Vec::new();
    for _ in 0..4 {
        let reader = image(&f, 4, 1, true).await;
        assert!(reader.headers().contains_key("content-length"));
        readers.push(reader);
    }
    let busy = image(&f, 17, 2, true)
        .await
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    assert_eq!(u32::from_le_bytes(busy[4..8].try_into().unwrap()), 0x40b);
    drop(readers.pop()); // HTTP cancellation releases its frame and admission.
    let recovered = image(&f, 17, 2, true).await;
    assert!(recovered.headers().contains_key("content-length"));
    drop(recovered);
    f.ok(
        "PUT",
        "/api/v1/camera/4/connected",
        "ClientID=1&Connected=false",
    )
    .await;
    assert_eq!(
        f.ok("GET", "/api/v1/camera/17/connected", "ClientID=2")
            .await,
        true
    );
    assert_eq!(
        f.call("GET", "/api/v1/camera/4/imageready", "ClientID=1")
            .await["ErrorNumber"],
        0x407
    );
    // Fully prepared bodies survive control disconnect with their own reservations.
    for reader in readers {
        assert!(reader.into_body().collect().await.unwrap().to_bytes().len() > 44);
    }
    assert_eq!(
        u32::from_le_bytes(
            image(&f, 17, 2, true)
                .await
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()[4..8]
                .try_into()
                .unwrap()
        ),
        0
    );
    f.finish().await;
}

#[tokio::test]
async fn camera_slot_conflicts_fail_discovery_routes_and_setup_without_connecting_equipment() {
    let f = Fixture::from_config(configuration()).await;
    for _ in 0..5 {
        f.server.profiles.add().unwrap();
    }
    assert_eq!(
        f.call("GET", "/management/v1/configureddevices", "").await["ErrorNumber"],
        0x401
    );
    assert_eq!(
        f.call("GET", "/api/v1/camera/4/name", "").await["ErrorNumber"],
        0x401
    );
    let page = f
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/setup/v1/camera/4/setup")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(page.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        f.hub
            .source_snapshots()
            .iter()
            .all(|source| source.lease_count == 0 && !source.transport_connected)
    );
    f.finish().await;
}

#[tokio::test]
async fn camera_async_connection_and_capture_survive_owner_disconnect_without_implicit_abort() {
    let f = Fixture::from_config(configuration()).await;
    f.ok("PUT", "/api/v1/camera/4/connect", "ClientID=1").await;
    tokio::time::timeout(Duration::from_secs(3), async {
        while f
            .ok("GET", "/api/v1/camera/4/connecting", "ClientID=1")
            .await
            == true
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        f.ok("GET", "/api/v1/camera/4/connected", "ClientID=1")
            .await,
        true
    );
    f.ok(
        "PUT",
        "/api/v1/camera/17/connected",
        "ClientID=2&Connected=true",
    )
    .await;
    assert_eq!(
        f.ok(
            "PUT",
            "/api/v1/camera/4/startexposure",
            "ClientID=1&Duration=0.3&Light=true"
        )
        .await,
        Value::Null
    );
    let capture = f
        .hub
        .camera_acquisition_status(f.config.sources[0].id)
        .unwrap()
        .acquisition
        .unwrap();
    f.ok(
        "PUT",
        "/api/v1/camera/4/connected",
        "ClientID=1&Connected=false",
    )
    .await;
    assert_eq!(
        f.call("PUT", "/api/v1/camera/17/abortexposure", "ClientID=2")
            .await["ErrorNumber"],
        0x40b
    );
    ready(&f, 17, 2).await;
    assert_eq!(
        f.hub
            .camera_acquisition_status(f.config.sources[0].id)
            .unwrap()
            .completed
            .unwrap()
            .acquisition,
        capture
    );
    assert!(
        image(&f, 17, 2, true)
            .await
            .headers()
            .contains_key("content-length")
    );
    f.finish().await;
}

#[tokio::test]
async fn camera_lost_write_acknowledgement_retains_shared_fence_without_replay() {
    let f = Fixture::from_config(configuration()).await;
    for (number, client) in [(4, 1), (17, 2)] {
        f.ok(
            "PUT",
            &format!("/api/v1/camera/{number}/connected"),
            &format!("ClientID={client}&Connected=true"),
        )
        .await;
    }
    f.hub
        .update_simulation(
            f.config.sources[0].id,
            serde_json::from_value(json!({"fault":"uncertainWrite"})).unwrap(),
        )
        .await
        .unwrap();
    let failure = f
        .call(
            "PUT",
            "/api/v1/camera/4/startexposure",
            "ClientID=1&Duration=0.1&Light=true",
        )
        .await;
    assert_eq!(failure["ErrorNumber"], 0x500);
    let snapshot = f.hub.source_snapshot(f.config.sources[0].id).unwrap();
    assert!(snapshot.write_uncertain);
    let acquisition = f
        .hub
        .camera_acquisition_status(f.config.sources[0].id)
        .unwrap();
    assert_eq!(
        acquisition.phase,
        regain_hub::camera::acquisition::AcquisitionPhase::Uncertain
    );
    // Even simulation edits cannot clear uncertainty while retained control is
    // held. Reconciliation must remain explicit, rather than replaying a write.
    assert_eq!(
        f.hub
            .update_simulation(
                f.config.sources[0].id,
                serde_json::from_value(json!({"fault":"none"})).unwrap()
            )
            .await
            .unwrap_err()
            .kind,
        regain_hub::source::ErrorKind::Busy
    );
    assert_eq!(
        f.call(
            "PUT",
            "/api/v1/camera/17/startexposure",
            "ClientID=2&Duration=0.1&Light=true"
        )
        .await["ErrorNumber"],
        0x407 // The lost write retired that source generation; it is not adopted.
    );
    assert_eq!(
        f.hub
            .camera_acquisition_status(f.config.sources[0].id)
            .unwrap()
            .acquisition,
        acquisition.acquisition
    );
    assert!(
        f.hub
            .source_snapshot(f.config.sources[0].id)
            .unwrap()
            .write_uncertain
    );
    f.finish().await;
}

#[tokio::test]
async fn native_sdk_and_direct_simulations_publish_shared_host_images_over_actual_http() {
    let Some(workers) = std::env::var_os("REGAIN_TEST_WORKERS") else {
        eprintln!(
            "Native camera publication requires REGAIN_TEST_WORKERS pointing to built production workers"
        );
        return;
    };
    for direct in [false, true] {
        let mut config = configuration();
        config.sources[0] = serde_json::from_value(json!({"id":config.sources[0].id,"label":"Explicit native camera simulation",
            "backend":{"kind":"native","device":if direct {"camera-direct"} else {"camera-sdk"},
                "identity":if direct {"direct-simulator"} else {"sim00001"},
                "camera":{"model":if direct {"ZWO ASI585MM Pro"} else {"ZWO Simulated"},
                    "recovery":{"maxRetries":0,"readyFrameDownloadRetries":0,"reconnectDelaySeconds":0.01}}},
            "polling":{"connectionTimeoutSeconds":10.0,"requestTimeoutSeconds":5.0,"pollSeconds":60.0}})).unwrap();
        let f = Fixture::from_config_with_workers(config, Some(workers.clone().into())).await;
        f.ok(
            "PUT",
            "/api/v1/camera/4/connected",
            "ClientID=1&Connected=true",
        )
        .await;
        f.ok(
            "PUT",
            "/api/v1/camera/17/connected",
            "ClientID=2&Connected=true",
        )
        .await;
        for (member, parameter) in [("numx", "NumX=64"), ("numy", "NumY=64")] {
            f.ok(
                "PUT",
                &format!("/api/v1/camera/4/{member}"),
                &format!("ClientID=1&{parameter}"),
            )
            .await;
        }
        f.ok(
            "PUT",
            "/api/v1/camera/4/startexposure",
            "ClientID=1&Duration=0.01&Light=true",
        )
        .await;
        ready(&f, 17, 2).await;
        let status = f
            .hub
            .camera_acquisition_status(f.config.sources[0].id)
            .unwrap();
        assert!(status.image_ready && status.completed.is_some());
        let wire = image(&f, 17, 2, true)
            .await
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let image = regain_hub::camera::image::read_imagebytes(
            &mut wire.as_ref(),
            &regain_hub::camera::image::ImageBudget::new(64 * 64 * 2).unwrap(),
            987,
        )
        .await
        .unwrap();
        assert_eq!(image.image.descriptor().width(), 64);
        assert_eq!(image.image.descriptor().height(), 64);
        assert_eq!(
            f.ok(
                "GET",
                "/api/v1/camera/17/lastexposureduration",
                "ClientID=2"
            )
            .await,
            0.01
        );
        f.finish().await;
    }
}
