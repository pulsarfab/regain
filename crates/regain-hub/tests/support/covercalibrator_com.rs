use super::*;
use regain_hub::covercalibrator::CoverCalibratorProperty as Property;

fn panel_config(source: SourceConfig) -> HubConfig {
    let mut config = HubConfig::empty();
    for number in [2, 17] {
        config.outputs.push(OutputConfig {
            id: Uuid::new_v4(),
            number,
            label: format!("Private COM panel {number}"),
            device: VirtualDevice::Proxy {
                source: source.id,
                device_type: DeviceType::CoverCalibrator,
            },
        });
    }
    config.sources.push(source);
    config
}
fn runtime(f: &Fixture, config: &HubConfig) -> Arc<HubRuntime> {
    HubRuntime::build(
        config.clone(),
        &f.native,
        &NoCredentials,
        Arc::new(MonotonicClock::default()),
    )
    .unwrap()
}

#[tokio::test]
async fn registered_panels_share_polling_leases_and_legacy_modern_completion() {
    let Some(f) = Fixture::load() else { return };
    for bitness in [Bitness::X86, Bitness::X64] {
        for version in [1, 2] {
            f.clear("Panel", json!({"version":version}));
            let source = f.source("Panel", DeviceType::CoverCalibrator, bitness);
            let id = source.id;
            let config = panel_config(source);
            let hub = runtime(&f, &config);
            assert_eq!(f.count("Panel", "Activate"), 0);
            let first = hub.client();
            let second = hub.client();
            first.connect(config.outputs[0].id).await.unwrap();
            second.connect(config.outputs[1].id).await.unwrap();
            let a = first.connection(config.outputs[0].id).unwrap();
            let b = second.connection(config.outputs[1].id).unwrap();
            let ap = a.covercalibrator().unwrap();
            let bp = b.covercalibrator().unwrap();
            assert_eq!(ap.generation(), bp.generation());
            assert_eq!(f.count("Panel", "Activate"), 1);
            assert_eq!(f.count("Panel", "Connect"), usize::from(version == 2));
            assert_eq!(f.count("Panel", "Connected.set"), usize::from(version == 1));
            until(|| {
                let state = hub.source_snapshot(id).unwrap();
                state.values.get("maxbrightness") == Some(&json!(4096))
                    && state.values.get("coverstate") == Some(&json!(1))
                    && state.values.get("calibratorstate") == Some(&json!(1))
            })
            .await;
            assert_eq!(
                ap.calibrator_on(4097).await.unwrap_err().kind,
                ErrorKind::InvalidValue
            );
            assert_eq!(f.count("Panel", "CalibratorOn"), 0);
            ap.calibrator_on(0).await.unwrap();
            assert_eq!(bp.property(Property::Brightness).await.unwrap(), json!(0));
            assert_eq!(
                bp.property(Property::CalibratorChanging).await.unwrap(),
                json!(true)
            );
            ap.open_cover().await.unwrap();
            assert_eq!(
                bp.property(Property::CoverMoving).await.unwrap(),
                json!(true)
            );
            bp.halt_cover().await.unwrap();
            assert_eq!(ap.property(Property::CoverState).await.unwrap(), json!(4));
            if version == 1 {
                assert_eq!(
                    ap.property(Property::CoverMoving).await.unwrap_err().kind,
                    ErrorKind::Unavailable
                );
            } else {
                assert_eq!(
                    ap.property(Property::CoverMoving).await.unwrap(),
                    json!(false)
                );
            }
            f.state("Panel", json!({"version":version,"panelMaxBrightness":17}));
            assert_eq!(
                ap.calibrator_on(18).await.unwrap_err().kind,
                ErrorKind::InvalidValue
            );
            bp.calibrator_on(17).await.unwrap();
            first.disconnect(config.outputs[0].id);
            drop(a);
            until(|| hub.source_snapshot(id).unwrap().lease_count == 1).await;
            assert!(bp.connected());
            assert_eq!(f.count("Panel", "Disconnect"), 0);
            second.disconnect(config.outputs[1].id);
            drop(b);
            hub.shutdown().await.unwrap();
            assert_eq!(f.count("Panel", "CalibratorOff"), 0);
            assert_eq!(f.count("Panel", "CloseCover"), 0);
            assert_eq!(f.count("Panel", "SetupDialog"), 0);
            assert_eq!(f.count("Panel", "Dispose"), 0);
        }
    }
}

#[tokio::test]
async fn registered_panel_readiness_keeps_modern_completion_mandatory_and_components_independent() {
    let Some(f) = Fixture::load() else { return };
    for bitness in [Bitness::X86, Bitness::X64] {
        for settings in [
            json!({"version":2,"panelCoverMoving":"false"}),
            json!({"version":2,"faultMember":"CalibratorChanging","faultCode":-2147220480}),
            json!({"version":2,"panelMaxBrightness":2147483648i64}),
        ] {
            f.clear("Panel", settings);
            let config = panel_config(f.source("Panel", DeviceType::CoverCalibrator, bitness));
            let hub = runtime(&f, &config);
            let client = hub.client();
            assert!(client.connect(config.outputs[0].id).await.is_err());
            for member in [
                "CalibratorOn",
                "CalibratorOff",
                "OpenCover",
                "CloseCover",
                "HaltCover",
            ] {
                assert_eq!(f.count("Panel", member), 0);
            }
            hub.shutdown().await.unwrap();
        }
        f.clear(
            "Panel",
            json!({"version":2,"panelCalibratorState":0,"panelMaxBrightness":0}),
        );
        let config = panel_config(f.source("Panel", DeviceType::CoverCalibrator, bitness));
        let hub = runtime(&f, &config);
        let client = hub.client();
        client.connect(config.outputs[0].id).await.unwrap();
        let connection = client.connection(config.outputs[0].id).unwrap();
        let panel = connection.covercalibrator().unwrap();
        assert_eq!(
            panel.calibrator_on(0).await.unwrap_err().kind,
            ErrorKind::Unsupported
        );
        panel.open_cover().await.unwrap();
        assert_eq!(f.count("Panel", "CalibratorOn"), 0);
        f.state("Panel", json!({"version":2,"panelCoverState":0}));
        assert_eq!(
            panel.close_cover().await.unwrap_err().kind,
            ErrorKind::Unsupported
        );
        panel.calibrator_on(0).await.unwrap();
        client.disconnect(config.outputs[0].id);
        drop(connection);
        hub.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn registered_panel_applied_lost_reply_fences_siblings_without_cleanup_actuation_or_replay() {
    let Some(f) = Fixture::load() else { return };
    for bitness in [Bitness::X86, Bitness::X64] {
        for version in [1, 2] {
            f.clear("Panel", json!({"version":version}));
            let source = f.source("Panel", DeviceType::CoverCalibrator, bitness);
            let id = source.id;
            let config = panel_config(source);
            let hub = runtime(&f, &config);
            let first = hub.client();
            let second = hub.client();
            first.connect(config.outputs[0].id).await.unwrap();
            second.connect(config.outputs[1].id).await.unwrap();
            let a = first.connection(config.outputs[0].id).unwrap();
            let b = second.connection(config.outputs[1].id).unwrap();
            assert_eq!(f.count("Panel", "Activate"), 1);
            f.state("Panel", json!({"version":version,"panelLoseOnReply":true}));
            assert_eq!(
                a.covercalibrator()
                    .unwrap()
                    .calibrator_on(17)
                    .await
                    .unwrap_err()
                    .kind,
                ErrorKind::Uncertain
            );
            assert_eq!(f.count("Panel", "CalibratorOn.applied"), 1);
            assert!(hub.source_snapshot(id).unwrap().write_uncertain);
            f.state("Panel", json!({"version":version}));
            let panel = b.covercalibrator().unwrap();
            assert_eq!(
                panel.calibrator_on(18).await.unwrap_err().kind,
                ErrorKind::Uncertain
            );
            assert_eq!(
                panel.calibrator_off().await.unwrap_err().kind,
                ErrorKind::Uncertain
            );
            assert_eq!(
                panel.open_cover().await.unwrap_err().kind,
                ErrorKind::Uncertain
            );
            assert_eq!(
                panel.close_cover().await.unwrap_err().kind,
                ErrorKind::Uncertain
            );
            assert_eq!(
                panel.halt_cover().await.unwrap_err().kind,
                ErrorKind::Uncertain
            );
            first.disconnect(config.outputs[0].id);
            second.disconnect(config.outputs[1].id);
            drop(a);
            drop(b);
            hub.shutdown().await.unwrap();
            // Poll recovery may activate a fresh read transport. Neither it nor
            // cleanup may replay the applied command or clear the shared fence.
            assert_eq!(f.count("Panel", "CalibratorOn"), 1);
            for member in [
                "CalibratorOff",
                "OpenCover",
                "CloseCover",
                "HaltCover",
                "Dispose",
            ] {
                assert_eq!(f.count("Panel", member), 0);
            }
        }
    }
}
