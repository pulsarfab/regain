use super::*;

#[tokio::test]
async fn http_syntax_is_rejected_before_connection_or_mutation_for_every_hub_class() {
    let mut config: HubConfig = serde_json::from_str(include_str!(
        "../../../regain-hub/examples/simulated-observatory.json"
    ))
    .unwrap();
    for output in &mut config.outputs {
        output.number = 40;
    }
    for kind in [
        "camera",
        "focuser",
        "rotator",
        "filterwheel",
        "covercalibrator",
    ] {
        let source = uuid::Uuid::new_v4();
        config.sources.push(
            serde_json::from_value(json!({
                "id":source,"label":"Explicit protocol simulation",
                "backend":{"kind":"simulated","deviceType":kind}
            }))
            .unwrap(),
        );
        config.outputs.push(
            serde_json::from_value(json!({
                "id":uuid::Uuid::new_v4(),"number":40,"label":"Explicit protocol output",
                "device":{"kind":"proxy","source":source,"deviceType":kind}
            }))
            .unwrap(),
        );
    }
    let f = Fixture::from_config(config).await;
    for kind in [
        "camera",
        "switch",
        "safetymonitor",
        "observingconditions",
        "focuser",
        "rotator",
        "filterwheel",
        "covercalibrator",
    ] {
        let path = format!("/api/v1/{kind}/40");
        for method in ["GET", "PUT"] {
            assert_eq!(
                request(&f.router, method, &format!("{path}/descrip"), "")
                    .await
                    .0,
                StatusCode::NOT_FOUND,
                "{kind} {method}"
            );
        }
        for body in [
            "connected=true",
            "CONNECTED=true",
            "Connected=true&connected=false",
            "",
        ] {
            assert_eq!(
                request(&f.router, "PUT", &format!("{path}/connected"), body)
                    .await
                    .0,
                StatusCode::BAD_REQUEST,
                "{kind} {body}"
            );
        }
        // GET keys remain case insensitive, including transaction round trips.
        let reply = f
            .call(
                "GET",
                &format!("{path}/connected"),
                "cLiEnTiD=7&CLIENTTRANSACTIONID=987",
            )
            .await;
        assert_eq!(reply["Value"], false);
        assert_eq!(reply["ClientTransactionID"], 987);
        for ids in ["ClientID=-1", "ClientID=", "ClientTransactionID=bad"] {
            assert_eq!(
                request(&f.router, "GET", &format!("{path}/connected"), ids)
                    .await
                    .0,
                StatusCode::BAD_REQUEST
            );
            assert_eq!(
                request(
                    &f.router,
                    "PUT",
                    &format!("{path}/connected"),
                    &format!("Connected=true&{ids}")
                )
                .await
                .0,
                StatusCode::BAD_REQUEST
            );
        }
    }
    let reply = f
        .call(
            "PUT",
            "/api/v1/safetymonitor/40/connected",
            "Connected=true&ClientID=7&clienttransactionid=987",
        )
        .await;
    assert_eq!(reply["ClientTransactionID"], 0);
    assert_eq!(
        f.ok("GET", "/api/v1/safetymonitor/40/connected", "ClientID=7")
            .await,
        true
    );
    f.ok(
        "PUT",
        "/api/v1/safetymonitor/40/connected",
        "Connected=false&ClientID=7",
    )
    .await;
    let reply = f
        .call(
            "PUT",
            "/api/v1/safetymonitor/40/connected",
            "Connected=true&clientid=7",
        )
        .await;
    assert_eq!(reply["ErrorNumber"], 0);
    assert_eq!(
        f.ok("GET", "/api/v1/safetymonitor/40/connected", "ClientID=7")
            .await,
        false
    );
    f.ok(
        "PUT",
        "/api/v1/safetymonitor/40/connected",
        "Connected=false",
    )
    .await;
    assert_eq!(
        request(
            &f.router,
            "GET",
            "/api/v1/safetymonitor/40/connected",
            "ClientID=7&CLIENTID=8"
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert!(
        f.hub
            .source_snapshots()
            .iter()
            .all(|s| s.lease_count == 0 && !s.transport_connected)
    );

    for (kind, member, before, bad_body) in [
        ("camera", "binx", "binx", "binx=2"),
        (
            "switch",
            "setswitchvalue",
            "getswitchvalue?Id=1",
            "Id=1&value=91",
        ),
        (
            "observingconditions",
            "averageperiod",
            "averageperiod",
            "averageperiod=1",
        ),
        ("focuser", "move", "position", "position=1"),
        ("rotator", "sync", "position", "position=1"),
        ("filterwheel", "position", "position", "position=1"),
        (
            "covercalibrator",
            "calibratoron",
            "calibratorstate",
            "brightness=1",
        ),
    ] {
        let path = format!("/api/v1/{kind}/40");
        f.ok("PUT", &format!("{path}/connected"), "Connected=true")
            .await;
        eventually(async || {
            f.call("GET", &format!("{path}/{before}"), "").await["ErrorNumber"] == 0
        })
        .await;
        let old = f.ok("GET", &format!("{path}/{before}"), "").await;
        assert_eq!(
            request(&f.router, "PUT", &format!("{path}/{member}"), bad_body)
                .await
                .0,
            StatusCode::BAD_REQUEST,
            "{kind}"
        );
        assert_eq!(
            f.ok("GET", &format!("{path}/{before}"), "").await,
            old,
            "{kind}"
        );
        f.ok("PUT", &format!("{path}/connected"), "Connected=false")
            .await;
    }
    // Recognised but unsupported methods remain ASCOM errors, not bad URLs.
    let unsupported = f
        .call(
            "PUT",
            "/api/v1/safetymonitor/40/action",
            "Action=Unknown&Parameters=",
        )
        .await;
    assert_eq!(unsupported["ErrorNumber"], 0x400);
    assert!(unsupported.get("Value").unwrap().is_null());
    assert_eq!(
        request(
            &f.router,
            "PUT",
            "/api/v1/switch/40/setswitchname",
            "Id=bogus&Name=ignored"
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    f.finish().await;
}
