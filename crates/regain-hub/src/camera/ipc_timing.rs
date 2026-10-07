//! Revision-bound frontend deadlines, derived from the controller's actual
//! allowances. These bounds neither change recovery policy nor authorize replay.
use crate::{
    host::Hello,
    ipc::{Command, Put},
    source::{ErrorKind, SourceError},
};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use uuid::Uuid;

// Leave room for two maximum (60-second) frontend frame allowances within the
// real net48 Task.Delay ceiling. Validated native policy maxima fit this bound.
pub const MAX_OPERATION_MILLISECONDS: u64 = i32::MAX as u64 - 120_000;

/// Inert duration-dependent capture bounds, not a capability to retry or replay.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CameraCaptureTiming {
    pub host_instance: Uuid,
    pub configuration_revision: Uuid,
    pub client_id: Uuid,
    pub output: Uuid,
    pub source: Uuid,
    pub native: bool,
    pub duration_seconds: f64,
    pub readiness_milliseconds: u64,
    pub completion_milliseconds: u64,
}
impl CameraCaptureTiming {
    pub(crate) fn matches(&self, hello: &Hello, output: Uuid, seconds: f64) -> bool {
        self.host_instance == hello.host_instance
            && self.configuration_revision == hello.configuration_revision
            && self.client_id == hello.client_id
            && self.output == output
            && !self.source.is_nil()
            && self.duration_seconds.is_finite()
            && self.duration_seconds >= 0.
            && self.duration_seconds == seconds
            && (1..=MAX_OPERATION_MILLISECONDS).contains(&self.readiness_milliseconds)
            && (self.readiness_milliseconds..=MAX_OPERATION_MILLISECONDS)
                .contains(&self.completion_milliseconds)
            && self.readiness_milliseconds as f64 >= seconds * 1000.
    }
}

#[derive(Clone, Copy)]
pub(crate) enum CameraOperation {
    Connect,
    Start,
    Setting,
    Stop,
    Abort,
}
pub(crate) fn operation(command: &Command) -> Option<(Uuid, CameraOperation)> {
    let (output, kind) = match command {
        Command::Connect { output }
        | Command::ChangeConnection {
            output,
            connected: true,
            asynchronous: false,
        } => (*output, CameraOperation::Connect),
        Command::Put { output, property } => (
            *output,
            match property {
                Put::StartExposure { .. } => CameraOperation::Start,
                Put::CameraSetting { .. } => CameraOperation::Setting,
                // Admission contains capability/completion preflight; the
                // remaining bound contains its write and first completion read.
                Put::PulseGuide { .. } => CameraOperation::Setting,
                Put::StopExposure {} => CameraOperation::Stop,
                Put::AbortExposure {} => CameraOperation::Abort,
                _ => return None,
            },
        ),
        _ => return None,
    };
    Some((output, kind))
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CameraOperationTiming {
    pub host_instance: Uuid,
    pub configuration_revision: Uuid,
    pub client_id: Uuid,
    pub output: Uuid,
    pub source: Uuid,
    pub native: bool,
    pub connect_milliseconds: u64,
    pub start_milliseconds: u64,
    pub setting_milliseconds: u64,
    pub stop_milliseconds: u64,
    pub abort_milliseconds: u64,
}
impl CameraOperationTiming {
    pub(crate) fn valid(&self) -> bool {
        [
            self.host_instance,
            self.configuration_revision,
            self.client_id,
            self.output,
            self.source,
        ]
        .iter()
        .all(|id| !id.is_nil())
            && [
                self.connect_milliseconds,
                self.start_milliseconds,
                self.setting_milliseconds,
                self.stop_milliseconds,
                self.abort_milliseconds,
            ]
            .iter()
            .all(|ms| (1..=MAX_OPERATION_MILLISECONDS).contains(ms))
    }
    pub(crate) fn matches(&self, hello: &Hello, output: Uuid) -> bool {
        self.valid()
            && self.host_instance == hello.host_instance
            && self.configuration_revision == hello.configuration_revision
            && self.client_id == hello.client_id
            && self.output == output
    }
    pub(crate) fn timeout(&self, kind: CameraOperation) -> Duration {
        Duration::from_millis(match kind {
            CameraOperation::Connect => self.connect_milliseconds,
            CameraOperation::Start => self.start_milliseconds,
            CameraOperation::Setting => self.setting_milliseconds,
            CameraOperation::Stop => self.stop_milliseconds,
            CameraOperation::Abort => self.abort_milliseconds,
        })
    }
}
pub(crate) fn milliseconds(duration: Duration, base: Duration) -> Result<u64, SourceError> {
    let value = u64::try_from(duration.max(base).as_nanos().div_ceil(1_000_000)).ok();
    value
        .filter(|ms| (1..=MAX_OPERATION_MILLISECONDS).contains(ms))
        .ok_or_else(|| {
            SourceError::new(
                ErrorKind::InvalidValue,
                "Camera frontend deadline is not representable",
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn descriptor_rejects_wrong_identities_unknown_fields_and_unrepresentable_timers() {
        let host = Uuid::new_v4();
        let revision = Uuid::new_v4();
        let client = Uuid::new_v4();
        let output = Uuid::new_v4();
        let hello = Hello {
            protocol_version: 1,
            instance_id: Uuid::new_v4(),
            host_instance: host,
            configuration_revision: revision,
            client_id: client,
            max_frame_bytes: 1024,
            max_in_flight: 2,
            operations: vec![],
            capabilities: vec![],
        };
        let value = json!({"hostInstance":host,"configurationRevision":revision,"clientId":client,"output":output,"source":Uuid::new_v4(),
            "native":false,"connectMilliseconds":1000,"startMilliseconds":1000,"settingMilliseconds":1000,"stopMilliseconds":1000,"abortMilliseconds":1000});
        let timing: CameraOperationTiming = serde_json::from_value(value.clone()).unwrap();
        assert!(timing.matches(&hello, output));
        for key in [
            "hostInstance",
            "configurationRevision",
            "clientId",
            "output",
            "source",
        ] {
            let mut bad = value.clone();
            bad[key] = json!(Uuid::nil());
            assert!(
                !serde_json::from_value::<CameraOperationTiming>(bad)
                    .unwrap()
                    .matches(&hello, output)
            );
        }
        for key in [
            "connectMilliseconds",
            "startMilliseconds",
            "settingMilliseconds",
            "stopMilliseconds",
            "abortMilliseconds",
        ] {
            for invalid in [
                json!(0),
                json!(-1),
                json!(1.5),
                json!(MAX_OPERATION_MILLISECONDS + 1),
            ] {
                let mut bad = value.clone();
                bad[key] = invalid;
                if let Ok(timing) = serde_json::from_value::<CameraOperationTiming>(bad) {
                    assert!(!timing.matches(&hello, output));
                }
            }
        }
        let mut bad = value;
        bad["extra"] = json!(1);
        assert!(serde_json::from_value::<CameraOperationTiming>(bad).is_err());
        assert_eq!(
            milliseconds(Duration::from_nanos(1), Duration::ZERO).unwrap(),
            1
        );
        assert_eq!(
            milliseconds(Duration::ZERO, Duration::from_secs(30)).unwrap(),
            30_000
        );
        assert!(
            milliseconds(
                Duration::from_millis(MAX_OPERATION_MILLISECONDS + 1),
                Duration::ZERO
            )
            .is_err()
        );
    }
}
