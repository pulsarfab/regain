//! Explicit worker simulation only; never discover or open attached equipment.
use regain_core::{CancellationToken, Exposure, RecoveryOptions, Runtime, Selection, Session};
use regain_hub::{
    camera::{
        image::{IMAGE_CHUNK_BYTES, ImageBudget, NATIVE_METADATA_BYTES},
        native_capture::{self, NativeCaptureError},
    },
    source::ErrorKind,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

fn exposure() -> Exposure {
    Exposure {
        width: 64,
        height: 64,
        bin: 1,
        x: 0,
        y: 0,
        microseconds: 10_000,
        dark: true,
    }
}
fn session(direct: bool, instant: bool, events: Arc<Mutex<Vec<String>>>) -> Session {
    Session::new(
        Selection {
            name: if direct {
                "ZWO ASI676MC"
            } else {
                "ZWO Simulated"
            }
            .into(),
            serial: None,
            direct,
            sdk_fallback: false,
            recovery: RecoveryOptions {
                reconnect_delay_seconds: 0.01,
                ..RecoveryOptions::default()
            },
        },
        Runtime {
            directory: std::env::var_os("REGAIN_TEST_WORKERS")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug")
                }),
            sdk: "unused".into(),
            simulate: true,
            sdk_simulation: Some(json!({"instant":instant})),
        },
        Arc::new(move |_, event, message| {
            events.lock().unwrap().push(format!("{event}: {message}"))
        }),
    )
    .unwrap()
}

#[tokio::test]
async fn native_capture_preserves_core_recovery_and_readers_block_new_admission() {
    for direct in [false, true] {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut session = session(direct, true, events.clone());
        let token = CancellationToken::new();
        session.connect(&token).await.unwrap();
        if direct {
            session.simulate_read_failures(2, &token).await.unwrap();
        }
        let e = exposure();
        let size = e.bytes().unwrap();
        let budget =
            ImageBudget::new(size * 2 + NATIVE_METADATA_BYTES + IMAGE_CHUNK_BYTES).unwrap();
        let image = native_capture::capture(&mut session, e.clone(), &budget, &token)
            .await
            .unwrap();
        assert_eq!(image.bytes().len(), size);
        assert_eq!(image.native().unwrap().exposure(), &e);
        let metadata: Value =
            serde_json::from_slice(image.native().unwrap().metadata_json()).unwrap();
        assert_eq!(metadata["backend"], if direct { "direct" } else { "sdk" });
        assert_eq!(metadata["exposure"], serde_json::to_value(&e).unwrap());
        assert_eq!(metadata["recoveries"], 0);
        assert_eq!(metadata["usbResets"], 0);
        assert_eq!(metadata["downloadRetries"], 0);
        if direct {
            assert_eq!(metadata["readRecoveries"], 2);
        }
        assert!(metadata["startedUtc"].is_string());
        assert!(metadata["endedUtc"].is_string());
        assert!(metadata["controls"].is_object());
        assert_eq!(budget.used_bytes(), size + NATIVE_METADATA_BYTES);
        let reader = image.clone();
        drop(image);
        let before = events.lock().unwrap().clone();
        let error = native_capture::capture(&mut session, e.clone(), &budget, &token)
            .await
            .err()
            .unwrap();
        assert!(
            matches!(error, NativeCaptureError::Contract(ref error) if error.kind == ErrorKind::Busy)
        );
        assert_eq!(
            *events.lock().unwrap(),
            before,
            "Rejected admission must send no exposure or recovery work"
        );
        assert_eq!(session.snapshot().phase, "Idle");
        assert_eq!(reader.bytes().len(), size);
        drop(reader);
        assert_eq!(budget.used_bytes(), 0);
        let image = native_capture::capture(&mut session, e, &budget, &token)
            .await
            .unwrap();
        drop(image);
        assert_eq!(budget.used_bytes(), 0);
        session.close().await;
    }
}

#[tokio::test]
async fn cancelled_native_capture_releases_admission_and_preserves_core_error() {
    let mut session = session(false, true, Arc::new(Mutex::new(Vec::new())));
    let connect = CancellationToken::new();
    session.connect(&connect).await.unwrap();
    let token = CancellationToken::new();
    let e = Exposure {
        microseconds: 5_000_000,
        ..exposure()
    };
    let budget =
        ImageBudget::new(e.bytes().unwrap() * 2 + NATIVE_METADATA_BYTES + IMAGE_CHUNK_BYTES)
            .unwrap();
    // Cancellation before dispatch must retain the core's typed cancellation.
    token.cancel();
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        native_capture::capture(&mut session, e, &budget, &token),
    )
    .await
    .unwrap()
    .err()
    .unwrap();
    assert!(
        matches!(error, NativeCaptureError::Capture(ref error) if matches!(error.downcast_ref::<regain_core::Failure>(), Some(regain_core::Failure::Cancelled)))
    );
    assert_eq!(budget.used_bytes(), 0);
    session.close().await;
}

#[tokio::test]
async fn native_capture_cancellation_during_exposure_releases_staging_and_output() {
    let mut session = session(false, false, Arc::new(Mutex::new(Vec::new())));
    let token = CancellationToken::new();
    session.connect(&token).await.unwrap();
    let status = session.status.clone();
    let e = Exposure {
        microseconds: 5_000_000,
        ..exposure()
    };
    let admitted = e.bytes().unwrap() * 2 + NATIVE_METADATA_BYTES + IMAGE_CHUNK_BYTES;
    let budget = ImageBudget::new(admitted).unwrap();
    let cancel = async {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if status.lock().unwrap().phase == "Exposing" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(budget.used_bytes(), admitted);
        token.cancel();
    };
    let (result, ()) = tokio::join!(
        native_capture::capture(&mut session, e, &budget, &token),
        cancel
    );
    assert!(
        matches!(result, Err(NativeCaptureError::Capture(ref error)) if matches!(error.downcast_ref::<regain_core::Failure>(), Some(regain_core::Failure::Cancelled)))
    );
    assert_eq!(budget.used_bytes(), 0);
    session.close().await;
}
