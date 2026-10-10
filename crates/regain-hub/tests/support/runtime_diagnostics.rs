use super::*;

fn status(f: &Fixture, output: Uuid, start: u32, limit: u32) -> Value {
    serde_json::to_value(f.runtime.output_status(output, start, limit).unwrap()).unwrap()
}
fn counts(f: &Fixture) -> Vec<(usize, usize, usize, usize, usize, usize)> {
    f.devices
        .iter()
        .enumerate()
        .map(|(index, device)| {
            (
                device.connects.load(SeqCst),
                device.disconnects.load(SeqCst),
                device.reads.load(SeqCst),
                device.writes.load(SeqCst),
                device.polls.load(SeqCst),
                f.registry
                    .get(f.config.sources[index].id)
                    .unwrap()
                    .snapshot()
                    .lease_count,
            )
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn cached_safe_from_another_owner_cannot_start_an_inactive_policy() {
    let f = fixture();
    let source = f.registry.get(f.config.sources[2].id).unwrap();
    let lease = Uuid::new_v4();
    source.acquire(lease).await.unwrap();
    settle().await;
    assert_eq!(source.snapshot().values["issafe"], true);
    let before = counts(&f);
    let observed = status(&f, f.safety, 0, 32);
    assert_eq!(observed["diagnostics"]["controllerActive"], false);
    assert_eq!(observed["diagnostics"]["isSafe"], false);
    assert_eq!(
        observed["diagnostics"]["members"][0]["decision"]["rawIsSafe"],
        Value::Null
    );
    assert_eq!(
        observed["diagnostics"]["members"][0]["decision"]["safeReadings"],
        0
    );
    assert_eq!(counts(&f), before);
    source.release(lease).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn safety_pagination_does_not_aggregate_only_the_visible_page() {
    let f = fixture_with(|config| {
        let mut source = config.sources[2].clone();
        source.id = Uuid::new_v4();
        if let regain_hub::config::SourceBackend::Alpaca { device_number, .. } = &mut source.backend
        {
            *device_number = 1;
        }
        let source_id = source.id;
        config.sources.push(source);
        if let VirtualDevice::Safety { members } = &mut config.outputs[1].device {
            let mut member = members[0].clone();
            member.source = source_id;
            members.push(member);
        }
    });
    f.devices[3].hang_poll.store(true, SeqCst);
    let client = f.runtime.client();
    client.connect(f.safety).await.unwrap();
    settle().await;
    let first = status(&f, f.safety, 0, 1);
    assert_eq!(first["nextStart"], 1);
    assert_eq!(
        first["diagnostics"]["members"][0]["decision"]["permitsSafe"],
        true
    );
    assert_eq!(first["diagnostics"]["isSafe"], false);
    let second = status(&f, f.safety, 1, 1);
    assert_eq!(
        second["diagnostics"]["members"][0]["decision"]["permitsSafe"],
        false
    );
    assert_eq!(second["diagnostics"]["isSafe"], false);
    client.close();
}

#[tokio::test(start_paused = true)]
async fn diagnostics_never_clear_or_replay_an_uncertain_write() {
    let f = fixture();
    let client = f.runtime.client();
    client.connect(f.switch).await.unwrap();
    settle().await;
    f.devices[0].hang_write.store(true, SeqCst);
    let connection = client.connection(f.switch).unwrap();
    let task = tokio::spawn(async move { connection.switch().unwrap().set_value(0, 0.0).await });
    settle().await;
    tokio::time::advance(Duration::from_secs(2)).await;
    assert_eq!(task.await.unwrap().unwrap_err().kind, ErrorKind::Uncertain);
    settle().await;
    let before = counts(&f);
    for _ in 0..100 {
        let observed = status(&f, f.switch, 0, 1);
        assert_eq!(
            observed["diagnostics"]["channels"][0]["health"]["writeUncertain"],
            true
        );
    }
    assert_eq!(counts(&f), before);
    assert_eq!(f.devices[0].writes.load(SeqCst), 1);
    client.close();
}

#[tokio::test(start_paused = true)]
async fn disconnected_diagnostics_are_bounded_and_do_not_start_any_equipment_or_policy() {
    let f = fixture();
    settle().await;
    let before = counts(&f);
    for _ in 0..20 {
        let safety = status(&f, f.safety, 0, 1);
        assert_eq!(safety["purpose"], "cachedDiagnostics");
        assert_eq!(safety["configurationRevision"], json!(f.config.revision));
        assert_eq!(safety["diagnostics"]["controllerActive"], false);
        assert_eq!(safety["diagnostics"]["isSafe"], false);
        let decision = &safety["diagnostics"]["members"][0]["decision"];
        assert_eq!(decision["phase"], "unknown");
        assert_eq!(decision["rawIsSafe"], Value::Null);
        assert_eq!(decision["safeReadings"], 0);
        let switch = status(&f, f.switch, 0, 1);
        assert_eq!(switch["total"], 2);
        assert_eq!(switch["nextStart"], 1);
        assert_eq!(
            switch["diagnostics"]["channels"].as_array().unwrap().len(),
            1
        );
        assert_eq!(
            switch["diagnostics"]["channels"][0]["sample"]["state"],
            "unavailable"
        );
        // Configuration intent never becomes operational CanWrite.
        assert_eq!(
            switch["diagnostics"]["channels"][0]["configuredWritable"],
            true
        );
        assert!(
            switch["diagnostics"]["channels"][0]
                .get("canWrite")
                .is_none()
        );
        let weather = status(&f, f.weather, 0, 32);
        assert_eq!(
            weather["diagnostics"]["measurements"][0]["sample"]["state"],
            "unavailable"
        );
    }
    assert_eq!(counts(&f), before);
    assert_eq!(f.runtime.active_connections(), 0);
    assert_eq!(status(&f, f.switch, 2, 1)["nextStart"], Value::Null);
    assert!(
        status(&f, f.switch, 2, 1)["diagnostics"]["channels"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    for (start, limit) in [(0, 0), (0, 33), (3, 1), (u32::MAX, 32)] {
        assert_eq!(
            f.runtime
                .output_status(f.switch, start, limit)
                .unwrap_err()
                .kind,
            ErrorKind::InvalidValue
        );
    }
    assert_eq!(
        f.runtime
            .output_status(Uuid::new_v4(), 0, 1)
            .unwrap_err()
            .kind,
        ErrorKind::InvalidValue
    );
    assert_eq!(counts(&f), before);
}

#[tokio::test(start_paused = true)]
async fn live_diagnostics_share_getter_freshness_without_leases_capability_reads_or_writes() {
    let f = fixture();
    let client = f.runtime.client();
    for output in [f.switch, f.weather, f.safety] {
        client.connect(output).await.unwrap();
    }
    settle().await;
    let before = counts(&f);
    let switches = client.connection(f.switch).unwrap();
    let weather = client.connection(f.weather).unwrap();
    let safety = client.connection(f.safety).unwrap();
    for _ in 0..20 {
        let switch = status(&f, f.switch, 0, 32);
        for index in 0..2 {
            assert_eq!(
                switch["diagnostics"]["channels"][index]["sample"]["reading"]["value"]
                    .as_f64()
                    .unwrap(),
                switches.switch().unwrap().value(index as u32).unwrap()
            );
        }
        let observed = status(&f, f.weather, 0, 32);
        assert_eq!(
            observed["diagnostics"]["measurements"][0]["sample"]["reading"]["value"]
                .as_f64()
                .unwrap(),
            weather
                .weather()
                .unwrap()
                .read(WeatherMetric::Temperature)
                .unwrap()
                .value
        );
        let live = serde_json::to_value(safety.safety().unwrap().snapshot()).unwrap();
        let observed = status(&f, f.safety, 0, 32);
        assert_eq!(observed["diagnostics"]["isSafe"], live["isSafe"]);
        assert_eq!(
            observed["diagnostics"]["members"][0]["decision"],
            live["endpoints"][f.config.sources[2].id.to_string()]
        );
    }
    assert_eq!(counts(&f), before);
    // Keep polling pending beyond evidence lifetime. Diagnostics must expire
    // permission even without another completed observation.
    for device in &f.devices {
        device.hang_poll.store(true, SeqCst);
    }
    tokio::time::advance(Duration::from_secs(4)).await;
    settle().await;
    let expired = status(&f, f.safety, 0, 32);
    assert_eq!(expired["diagnostics"]["isSafe"], false);
    assert_eq!(
        expired["diagnostics"]["members"][0]["decision"]["permitsSafe"],
        false
    );
    assert_eq!(
        status(&f, f.switch, 0, 32)["diagnostics"]["channels"][0]["sample"]["state"],
        "unavailable"
    );
    client.close();
}

#[tokio::test(start_paused = true)]
async fn diagnostic_reads_cannot_accelerate_safety_recovery_or_restore_expired_evidence() {
    let f = fixture_with(|config| {
        if let VirtualDevice::Safety { members } = &mut config.outputs[1].device {
            members[0].policy.safe_readings_to_safe = 3;
            members[0].policy.return_to_safe_hold_seconds = 2.0;
        }
    });
    let client = f.runtime.client();
    client.connect(f.safety).await.unwrap();
    settle().await;
    let first = status(&f, f.safety, 0, 32);
    assert_eq!(first["diagnostics"]["isSafe"], false);
    assert_eq!(
        first["diagnostics"]["members"][0]["decision"]["safeReadings"],
        1
    );
    for _ in 0..100 {
        assert_eq!(status(&f, f.safety, 0, 32), first);
    }
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    let second = status(&f, f.safety, 0, 32);
    assert_eq!(second["diagnostics"]["isSafe"], false);
    assert_eq!(
        second["diagnostics"]["members"][0]["decision"]["safeReadings"],
        2
    );
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert_eq!(status(&f, f.safety, 0, 32)["diagnostics"]["isSafe"], true);
    f.devices[2].hang_poll.store(true, SeqCst);
    tokio::time::advance(Duration::from_secs(4)).await;
    settle().await;
    for _ in 0..100 {
        assert_eq!(status(&f, f.safety, 0, 32)["diagnostics"]["isSafe"], false);
    }
    client.close();
}

#[tokio::test(start_paused = true)]
async fn diagnostics_keep_reserved_switch_slots_and_weather_fallback_failures_independent() {
    let f = fixture_with(|config| {
        if let VirtualDevice::Switch { channels } = &mut config.outputs[0].device {
            channels[1].number = 1023;
        }
        let source = config.sources[0].id;
        if let VirtualDevice::Weather { measurements } = &mut config.outputs[2].device {
            measurements
                .get_mut(&WeatherMetric::Temperature)
                .unwrap()
                .sources
                .push(Readout::Channel {
                    source,
                    channel: 0,
                    unit: Some("°C".into()),
                });
            measurements
                .get_mut(&WeatherMetric::Temperature)
                .unwrap()
                .maximum_age_seconds = 0.5;
            measurements.insert(
                WeatherMetric::Pressure,
                Measurement {
                    sources: vec![Readout::Channel {
                        source,
                        channel: 0,
                        unit: Some("hPa".into()),
                    }],
                    maximum_age_seconds: 10.0,
                    average_seconds: 0.0,
                },
            );
        }
    });
    let first = status(&f, f.switch, 0, 32);
    assert_eq!(first["total"], 1024);
    assert_eq!(first["nextStart"], 32);
    assert_eq!(
        first["diagnostics"]["channels"][1],
        json!({"state":"removed","number":1})
    );
    let last = status(&f, f.switch, 1023, 32);
    assert_eq!(last["nextStart"], Value::Null);
    assert_eq!(last["diagnostics"]["channels"][0]["number"], 1023);
    let client = f.runtime.client();
    client.connect(f.weather).await.unwrap();
    settle().await;
    f.devices[1].hang_poll.store(true, SeqCst);
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    let fallback = status(&f, f.weather, 0, 32);
    let measurements = fallback["diagnostics"]["measurements"].as_array().unwrap();
    let temperature = measurements
        .iter()
        .find(|item| item["metric"] == "temperature")
        .unwrap();
    assert_eq!(
        temperature["sample"]["reading"]["source"],
        json!(f.config.sources[0].id)
    );
    assert_eq!(temperature["sample"]["reading"]["value"], 1.0);
    f.devices[0].hang_poll.store(true, SeqCst);
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    let failed = status(&f, f.weather, 0, 32);
    let measurements = failed["diagnostics"]["measurements"].as_array().unwrap();
    assert_eq!(
        measurements
            .iter()
            .find(|item| item["metric"] == "temperature")
            .unwrap()["sample"]["state"],
        "unavailable"
    );
    assert_eq!(
        measurements
            .iter()
            .find(|item| item["metric"] == "pressure")
            .unwrap()["sample"]["state"],
        "available"
    );
    client.close();
}
