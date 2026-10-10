//! Exact-acquisition composition; an inner output never supplies a later frame
//! under the identity of the exposure this virtual transport acknowledged.
use crate::{
    camera::{
        acquisition::{AcquisitionPhase, CameraSession, CapturedImage, ExposureRequest},
        image::{CameraImage, ImageBudget},
        properties::{CameraProperty, CameraSetting},
    },
    source::{ErrorKind, SourceError, Values},
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

#[derive(Default)]
pub(super) struct Camera {
    acquisition: Option<Uuid>,
    pinned: Option<Arc<CapturedImage>>,
}
fn unavailable() -> SourceError {
    SourceError::new(
        ErrorKind::Unavailable,
        "The virtual camera acquisition is unavailable or has changed",
    )
}
impl Camera {
    pub(super) fn clear(&mut self) {
        self.acquisition = None;
        self.pinned = None;
    }
    fn pin(&mut self, session: &CameraSession) -> Result<bool, SourceError> {
        if self.pinned.is_some() {
            return Ok(true);
        }
        let Some(acquisition) = self.acquisition else {
            return Ok(false);
        };
        let status = session.status();
        if status
            .completed
            .as_ref()
            .is_some_and(|completed| completed.acquisition == acquisition)
        {
            let image = session.image()?;
            // Another inner client can start between the status and image reads.
            if image.identity.acquisition != acquisition {
                return Err(unavailable());
            }
            self.pinned = Some(image);
            return Ok(true);
        }
        if status.acquisition != Some(acquisition) {
            return Err(unavailable());
        }
        if status.phase == AcquisitionPhase::Uncertain {
            return Err(status.error.unwrap_or_else(SourceError::uncertain));
        }
        Ok(false)
    }
    pub(super) async fn read(
        &mut self,
        session: &CameraSession,
        member: &str,
        args: &Values,
    ) -> Result<Value, SourceError> {
        super::no_args(args)?;
        let property = CameraProperty::from_member(member).ok_or_else(super::unsupported)?;
        if property == CameraProperty::ImageReady {
            return self.pin(session).map(|ready| json!(ready));
        }
        if matches!(
            property,
            CameraProperty::LastExposureDuration | CameraProperty::LastExposureStartTime
        ) {
            if !self.pin(session)? {
                return Err(unavailable());
            }
            let exposure = &self
                .pinned
                .as_ref()
                .expect("Pinned acquisition")
                .identity
                .exposure;
            return match property {
                CameraProperty::LastExposureDuration => exposure
                    .duration_seconds
                    .map(|value| json!(value))
                    .ok_or_else(|| exposure.duration_error.clone().unwrap_or_else(unavailable)),
                CameraProperty::LastExposureStartTime => exposure
                    .start_time
                    .as_ref()
                    .map(|value| json!(value))
                    .ok_or_else(|| {
                        exposure
                            .start_time_error
                            .clone()
                            .unwrap_or_else(unavailable)
                    }),
                _ => unreachable!(),
            };
        }
        session
            .property(property)
            .await
            .map(|value| value.into_value())
    }
    pub(super) async fn write(
        &mut self,
        session: &CameraSession,
        member: &str,
        args: &Values,
    ) -> Result<(), SourceError> {
        match member {
            "pulseguide" => {
                if let Some(active) = session.status().acquisition
                    && self.acquisition != Some(active)
                {
                    return Err(unavailable());
                }
                session
                    .pulse_guide(crate::camera::acquisition::GuideRequest::from_parameters(
                        args,
                    )?)
                    .await?;
            }
            "startexposure" => {
                if args.len() != 2 {
                    return Err(super::invalid("Expected Duration and Light"));
                }
                let seconds = args
                    .get("Duration")
                    .and_then(Value::as_f64)
                    .ok_or_else(|| super::invalid("Expected numeric Duration"))?;
                let light = args
                    .get("Light")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| super::invalid("Expected boolean Light"))?;
                Duration::try_from_secs_f64(seconds)
                    .map_err(|_| super::invalid("Invalid exposure duration"))?;
                let acquisition = session
                    .start(ExposureRequest {
                        duration_seconds: seconds,
                        light,
                    })
                    .await?;
                self.acquisition = Some(acquisition);
                self.pinned = None;
            }
            "abortexposure" | "stopexposure" => {
                super::no_args(args)?;
                // Never act on a later exposure, even if it was started using
                // the same shared inner source through a different output.
                if let Some(active) = session.status().acquisition
                    && self.acquisition != Some(active)
                {
                    return Err(unavailable());
                }
                if member == "abortexposure" {
                    session.abort().await?;
                    self.clear();
                } else {
                    session.stop().await?;
                }
            }
            _ => {
                session
                    .set(CameraSetting::from_parameters(member, args)?)
                    .await?
            }
        }
        Ok(())
    }
    pub(super) fn image(
        &mut self,
        session: &CameraSession,
        budget: &ImageBudget,
    ) -> Result<CameraImage, SourceError> {
        if !self.pin(session)? {
            return Err(unavailable());
        }
        let image = &self.pinned.as_ref().expect("Pinned acquisition").image;
        if !image.shares_budget(budget) {
            return Err(SourceError::new(
                ErrorKind::InvalidValue,
                "Virtual cameras must share the host image budget",
            ));
        }
        Ok(image.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::HubConfig, factory::NoCredentials, native::NativeRuntime, runtime::HubRuntime,
        safety::MonotonicClock,
    };
    fn runtime() -> (Arc<HubRuntime>, Uuid, ImageBudget) {
        let mut config = HubConfig::empty();
        let source = Uuid::new_v4();
        let output = Uuid::new_v4();
        config.sources.push(
            serde_json::from_value(json!({"id":source,"label":"Private pin test",
            "backend":{"kind":"simulated","deviceType":"camera"}}))
            .unwrap(),
        );
        config.outputs.push(
            serde_json::from_value(json!({"id":output,"number":0,"label":"Private pin output",
            "device":{"kind":"proxy","source":source,"deviceType":"camera"}}))
            .unwrap(),
        );
        let resources = crate::camera::runtime::CameraResources::new(2 * 1024 * 1024).unwrap();
        let budget = resources.image_budget();
        let runtime = HubRuntime::build_with_camera_resources(
            config,
            &NativeRuntime {
                directory: "absent-pin-workers".into(),
                simulate: false,
                references: None,
                cameras: None,
            },
            &NoCredentials,
            Arc::new(MonotonicClock::default()),
            resources,
        )
        .unwrap();
        (runtime, output, budget)
    }
    async fn completed(session: &CameraSession, acquisition: Uuid) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while !session
                .status()
                .completed
                .is_some_and(|image| image.acquisition == acquisition)
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
    fn exposure(seconds: f64) -> Values {
        Values::from([
            ("Duration".into(), json!(seconds)),
            ("Light".into(), json!(true)),
        ])
    }
    #[tokio::test(start_paused = true)]
    async fn replacement_before_pin_cannot_publish_or_stop_the_wrong_acquisition() {
        let (runtime, output, budget) = runtime();
        let client = runtime.client();
        client.connect(output).await.unwrap();
        let connection = client.connection(output).unwrap();
        let session = connection.camera().unwrap();
        let mut camera = Camera::default();
        camera
            .write(session, "startexposure", &exposure(0.1))
            .await
            .unwrap();
        completed(session, camera.acquisition.unwrap()).await;
        let newer = session
            .start(ExposureRequest {
                duration_seconds: 0.1,
                light: true,
            })
            .await
            .unwrap();
        completed(session, newer).await;
        assert_eq!(
            camera
                .read(session, "imageready", &Values::new())
                .await
                .unwrap_err()
                .kind,
            ErrorKind::Unavailable
        );
        assert_eq!(
            camera.image(session, &budget).err().unwrap().kind,
            ErrorKind::Unavailable
        );
        assert!(camera.pinned.is_none());
        // Use the same inner session, so its ownership guard alone would allow
        // aborting this later exposure. The virtual acquisition fence must stop it.
        let active = session
            .start(ExposureRequest {
                duration_seconds: 10.0,
                light: true,
            })
            .await
            .unwrap();
        for command in ["abortexposure", "stopexposure"] {
            assert_eq!(
                camera
                    .write(session, command, &Values::new())
                    .await
                    .unwrap_err()
                    .kind,
                ErrorKind::Unavailable
            );
            assert_eq!(session.status().acquisition, Some(active));
        }
        session.abort().await.unwrap();
        client.close();
        drop(connection);
        runtime.shutdown().await.unwrap();
        assert_eq!(budget.used_bytes(), 0);
    }
    #[tokio::test(start_paused = true)]
    async fn pin_freezes_metadata_pixels_and_budget_identity_across_inner_replacement() {
        let (runtime, output, budget) = runtime();
        let client = runtime.client();
        client.connect(output).await.unwrap();
        let connection = client.connection(output).unwrap();
        let session = connection.camera().unwrap();
        let mut camera = Camera::default();
        camera
            .write(session, "startexposure", &exposure(0.1))
            .await
            .unwrap();
        completed(session, camera.acquisition.unwrap()).await;
        assert_eq!(
            camera
                .read(session, "imageready", &Values::new())
                .await
                .unwrap(),
            true
        );
        let old = camera.image(session, &budget).unwrap();
        let start = camera
            .read(session, "lastexposurestarttime", &Values::new())
            .await
            .unwrap();
        let newer = session
            .start(ExposureRequest {
                duration_seconds: 0.2,
                light: true,
            })
            .await
            .unwrap();
        completed(session, newer).await;
        assert_eq!(
            camera
                .read(session, "lastexposureduration", &Values::new())
                .await
                .unwrap(),
            0.1
        );
        assert_eq!(
            camera
                .read(session, "lastexposurestarttime", &Values::new())
                .await
                .unwrap(),
            start
        );
        assert_eq!(
            camera.image(session, &budget).unwrap().bytes().as_ptr(),
            old.bytes().as_ptr()
        );
        let other = ImageBudget::new(2 * 1024 * 1024).unwrap();
        assert_eq!(
            camera.image(session, &other).err().unwrap().kind,
            ErrorKind::InvalidValue
        );
        assert_eq!(other.used_bytes(), 0);
        camera.clear();
        drop(old);
        client.close();
        drop(connection);
        runtime.shutdown().await.unwrap();
        assert_eq!(budget.used_bytes(), 0);
    }
}
