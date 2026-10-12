//! SDK API arguments and scalar results only; never inspect a frame buffer.
use crate::asi::diagnostics::queued_details;
use serde_json::{Value, json};
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

pub(super) struct ApiTrace {
    id: u64,
    api: &'static str,
    arguments: Value,
}
impl ApiTrace {
    pub fn start(api: &'static str, arguments: Value) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let trace = Self {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            api,
            arguments,
        };
        queued_details(
            "info",
            "sdk.api.start",
            format_args!("SDK {api} start"),
            json!({"api":api,"correlation":trace.id,"arguments":trace.arguments}),
        );
        trace
    }
    pub fn complete(self, result: Value, elapsed: Duration) {
        queued_details(
            "info",
            "sdk.api.complete",
            format_args!("SDK {} complete", self.api),
            self.record(result, elapsed),
        );
    }
    fn record(&self, result: Value, elapsed: Duration) -> Value {
        json!({"api":self.api,"correlation":self.id,"arguments":self.arguments,
            "elapsedUs":elapsed.as_micros(),"result":result})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn api_records_preserve_inputs_outputs_and_duration_without_buffers() {
        let trace = ApiTrace {
            id: 7,
            api: "ASIGetControlValue",
            arguments: json!({"cameraId":0,"control":17}),
        };
        let record = trace.record(
            json!({"returnValue":0,"outputs":{"value":1,"auto":false}}),
            Duration::from_micros(123),
        );
        assert_eq!(record["elapsedUs"], 123);
        assert_eq!(record["arguments"]["control"], 17);
        assert_eq!(record["result"]["outputs"]["value"], 1);
        assert!(!record.to_string().contains("buffer"));
    }
}
