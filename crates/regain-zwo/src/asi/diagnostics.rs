//! Log records use stderr only; they never change command replies or pixels.
use std::{
    fmt::Display,
    io::Write,
    sync::{
        OnceLock,
        atomic::{AtomicU8, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

enum UsbRecord {
    Line(String),
    Flush(mpsc::Sender<()>),
}

pub(super) fn command_logging_enabled() -> bool {
    log_mode() != 0
}
pub(super) fn forced_off() -> bool {
    !enabled_value(std::env::var("REGAIN_CAMERA_TRANSFER_LOG").ok().as_deref())
}
fn mode_cell() -> &'static AtomicU8 {
    static MODE: OnceLock<AtomicU8> = OnceLock::new();
    MODE.get_or_init(|| {
        AtomicU8::new(if forced_off() {
            0
        } else {
            match std::env::var("REGAIN_CAMERA_TRANSFER_LOG_MODE").as_deref() {
                Ok("off") => 0,
                Ok("verbose") => 2,
                _ => 1,
            }
        })
    })
}
fn log_mode() -> u8 {
    mode_cell().load(Ordering::Relaxed)
}
pub(super) fn configure(mode: &str) -> anyhow::Result<()> {
    let mode = match mode {
        "off" => 0,
        "circular" => 1,
        "verbose" => 2,
        _ => anyhow::bail!("Invalid transfer log mode"),
    };
    mode_cell().store(if forced_off() { 0 } else { mode }, Ordering::Relaxed);
    queued_details(
        "info",
        "diagnostics.mode",
        "Camera transfer logging updated",
        serde_json::json!({"mode":match log_mode(){0=>"off",1=>"circular",_=>"verbose"},"environmentOverride":forced_off()}),
    );
    Ok(())
}

fn enabled_value(value: Option<&str>) -> bool {
    !value.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off"
        )
    })
}

fn command_event(event: &str) -> bool {
    event.starts_with("usb.")
        || event.starts_with("sdk.api.")
        || event == "asi6200.register_snapshot"
}

fn failure_snapshot(event: &str, details: &serde_json::Value) -> bool {
    event == "asi6200.register_snapshot"
        && matches!(
            details["stage"].as_str(),
            Some("read_failure" | "capture_failure")
        )
}
static DROPPED_USB_RECORDS: AtomicU64 = AtomicU64::new(0);

/// The capture owner never takes a logfile/stderr lock while DMA is pending.
/// Formatting is bounded to metadata; the writer owns all potentially blocking
/// stderr I/O. A device close flushes this queue only after transfers drain.
fn usb_writer() -> &'static mpsc::SyncSender<UsbRecord> {
    static WRITER: OnceLock<mpsc::SyncSender<UsbRecord>> = OnceLock::new();
    WRITER.get_or_init(|| {
        // Bound memory even if the receiving application stops reading stderr.
        // Capture never blocks on the logger; exceptional loss is explicit.
        let (sender, receiver) = mpsc::sync_channel(8192);
        let _ = std::thread::Builder::new()
            .name("regain-usb-log".into())
            .spawn(move || {
                let mut recorder: Option<super::flight_recorder::Recorder> = None;
                for record in receiver {
                    let dropped = DROPPED_USB_RECORDS.swap(0, Ordering::Relaxed);
                    if dropped != 0 {
                        write(&mut std::io::stderr().lock(), dropped_record(dropped));
                    }
                    match record {
                        UsbRecord::Line(line) => {
                            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {continue};
                            let mode = log_mode();
                            let mut after_fault = None;
                            if mode == 1 {
                                let recorder = recorder.get_or_insert_with(|| {
                                    let recorder = super::flight_recorder::Recorder::open();
                                    write(&mut std::io::stderr().lock(), serde_json::json!({"version":1,"level":"info","event":"diagnostics.recorder","message":"Circular camera flight recorder enabled","pid":std::process::id(),"details":{"path":recorder.path(),"memoryBytes":8388608,"diskSegmentBytes":4194304,"diskSegments":2}}));
                                    recorder
                                });
                                recorder.remember(&line);
                                if super::flight_recorder::fault(&value) {
                                    for item in recorder.fault(&value, Instant::now()) {write(&mut std::io::stderr().lock(), item);}
                                }
                                after_fault = recorder.after_fault(Instant::now());
                            } else if let Some(recorder) = &mut recorder { recorder.clear(); }
                            if let Some(batch) = after_fault.filter(|_| !super::flight_recorder::fault(&value)) {
                                let mut value = value; value["flightRecorderBatch"] = serde_json::json!(batch);
                                value["flightRecorderAfterFault"] = serde_json::json!(true);
                                write(&mut std::io::stderr().lock(), value);
                            } else if mode == 2 || !command_event(value["event"].as_str().unwrap_or("")) || super::flight_recorder::fault(&value) {
                                write(&mut std::io::stderr().lock(), line);
                            }
                        }
                        UsbRecord::Flush(done) => {
                            if let Some(recorder) = &mut recorder {recorder.flush();}
                            let _ = done.send(());
                        }
                    }
                }
            });
        sender
    })
}

pub(super) fn flush_usb() {
    let (sender, receiver) = mpsc::channel();
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut record = UsbRecord::Flush(sender);
    loop {
        match usb_writer().try_send(record) {
            Ok(()) => {
                let _ = receiver.recv_timeout(deadline.saturating_duration_since(Instant::now()));
                return;
            }
            Err(mpsc::TrySendError::Full(unsent)) if Instant::now() < deadline => {
                record = unsent;
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(_) => return,
        }
    }
}

pub(crate) fn queued_details(
    level: &str,
    event: &str,
    message: impl Display,
    mut details: serde_json::Value,
) {
    if command_event(event)
        && !command_logging_enabled()
        && !matches!(level, "warning" | "error")
        && !failure_snapshot(event, &details)
    {
        return;
    }
    details["timestampUnixUs"] = serde_json::json!(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros()
    );
    details["thread"] = serde_json::json!(format!("{:?}", std::thread::current().id()));
    enqueue(serde_json::json!({"version":1,"level":level,"event":event,
        "message":message.to_string(),"pid":std::process::id(),"details":details}));
}

fn enqueue(record: serde_json::Value) {
    enqueue_to(usb_writer(), &DROPPED_USB_RECORDS, record);
}

fn enqueue_to(
    sender: &mpsc::SyncSender<UsbRecord>,
    dropped: &AtomicU64,
    record: serde_json::Value,
) {
    if sender
        .try_send(UsbRecord::Line(record.to_string()))
        .is_err()
    {
        dropped.fetch_add(1, Ordering::Relaxed);
    }
}

fn dropped_record(dropped: u64) -> serde_json::Value {
    serde_json::json!({"version":1,"level":"warning","event":"usb.log_dropped",
        "message":format!("USB diagnostic queue full: {dropped} records dropped; transfer behavior unchanged"),
        "pid":std::process::id(),"details":{"droppedRecords":dropped,"queueCapacity":8192}})
}

/// One operation's lifetime spans submission through terminal completion, even
/// when completion is harvested long after its kernel request was submitted.
#[cfg(any(feature = "asi-direct", windows, test))]
pub(super) struct UsbTrace {
    id: u64,
    started: Instant,
    fields: serde_json::Value,
}
#[cfg(any(feature = "asi-direct", windows, test))]
impl UsbTrace {
    #[cfg(any(feature = "asi-direct", windows))]
    pub fn start(operation: &str, mut fields: serde_json::Value) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        fields["operation"] = serde_json::json!(operation);
        let trace = Self {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            started: Instant::now(),
            fields,
        };
        trace.event("submit", serde_json::json!({}));
        trace
    }
    #[cfg(any(feature = "asi-direct", windows))]
    pub fn event(&self, stage: &str, fields: serde_json::Value) {
        if !command_logging_enabled() {
            return;
        }
        let record = self.record(stage, fields);
        enqueue(record);
    }
    fn record(&self, stage: &str, fields: serde_json::Value) -> serde_json::Value {
        let mut details = self.fields.clone();
        if let Some(fields) = fields.as_object() {
            for (key, value) in fields {
                details[key] = value.clone();
            }
        }
        details["correlation"] = serde_json::json!(self.id);
        details["stage"] = serde_json::json!(stage);
        details["elapsedUs"] = serde_json::json!(self.started.elapsed().as_micros());
        details["timestampUnixUs"] = serde_json::json!(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_micros()
        );
        details["thread"] = serde_json::json!(format!("{:?}", std::thread::current().id()));
        serde_json::json!({"version":1,"level":"info","event":format!("usb.{stage}"),
            "message":format!("USB {} {stage}",details["operation"].as_str().unwrap_or("operation")),
            "pid":std::process::id(),"details":details})
    }
}

#[cfg(any(feature = "asi-direct", test))]
pub(super) fn control_fields(
    request: u8,
    value: u16,
    index: u16,
    length: u16,
) -> serde_json::Value {
    serde_json::json!({"request":format!("0x{request:02x}"), "wValue":format!("0x{value:04x}"),
        "wIndex":format!("0x{index:04x}"), "direction":if length == 0 {"out"} else {"in"},
        "requestedBytes":length})
}

/// Only decoded operational register replies are loggable. Identity, EEPROM,
/// calibration and image-bearing commands are never included, even if short.
#[cfg(any(feature = "asi-direct", test))]
pub(super) fn control_payload(request: u8, length: u16, bytes: &[u8]) -> serde_json::Value {
    if matches!(request, 0xbc | 0xb3)
        && (1..=4).contains(&length)
        && bytes.len() == usize::from(length)
    {
        serde_json::json!({"controlValue":bytes.iter().enumerate().fold(0_u32, |value,(index,byte)| value | (u32::from(*byte) << (index * 8)))})
    } else {
        serde_json::json!({})
    }
}

#[cfg(feature = "asi-direct")]
pub fn log(level: &str, event: &str, message: impl Display) {
    queued_details(level, event, message, serde_json::json!({}));
}

#[cfg(feature = "asi-direct")]
pub fn details(level: &str, event: &str, message: impl Display, details: serde_json::Value) {
    queued_details(level, event, message, details);
}

fn write(out: &mut impl Write, record: impl Display) {
    // Diagnostics are best effort, including a closed or failed stderr pipe.
    let _ = writeln!(out, "REGAIN_DIAGNOSTIC {record}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_log_switch_defaults_on_and_only_suppresses_verbose_families() {
        for value in [None, Some("1"), Some("true"), Some("on"), Some("")] {
            assert!(enabled_value(value));
        }
        for value in [Some("0"), Some("FALSE"), Some(" off ")] {
            assert!(!enabled_value(value));
        }
        assert!(command_event("usb.complete"));
        assert!(command_event("sdk.api.start"));
        assert!(!command_event("capture.phase"));
        assert!(!command_event("capture.failed"));
        assert!(!command_event("transfer.retry"));
        assert!(failure_snapshot(
            "asi6200.register_snapshot",
            &serde_json::json!({"stage":"read_failure"})
        ));
        assert!(failure_snapshot(
            "asi6200.register_snapshot",
            &serde_json::json!({"stage":"capture_failure"})
        ));
        assert!(!failure_snapshot(
            "asi6200.register_snapshot",
            &serde_json::json!({"stage":"before_start"})
        ));
    }

    #[test]
    fn operational_values_are_bounded_and_identity_calibration_and_images_are_omitted() {
        assert_eq!(
            control_payload(0xbc, 2, &[0x34, 0x12])["controlValue"],
            0x1234
        );
        assert_eq!(
            control_payload(0xb3, 4, &[1, 2, 3, 4])["controlValue"],
            0x04030201
        );
        for request in [0xc8, 0xc3, 0xaf, 0xa9, 6] {
            assert_eq!(
                control_payload(request, 2, &[0x34, 0x12]),
                serde_json::json!({})
            );
        }
        assert_eq!(control_payload(0xbc, 5, &[1; 5]), serde_json::json!({}));
        assert_eq!(control_payload(0xbc, 4, &[1; 2]), serde_json::json!({}));
        let setup = control_fields(0xb6, 0x0187, 4, 0);
        assert_eq!(setup["request"], "0xb6");
        assert_eq!(setup["wValue"], "0x0187");
        assert_eq!(setup["wIndex"], "0x0004");
        assert_eq!(setup["direction"], "out");
    }

    #[test]
    fn full_sensor_default_queue_keeps_every_correlated_transfer_record_without_sampling() {
        let (sender, receiver) = mpsc::sync_channel(8192);
        let dropped = AtomicU64::new(0);
        let frame = 9576_usize * 6388 * 2;
        let chunk = 1024 * 1024;
        for number in 0..frame.div_ceil(chunk) {
            let trace = UsbTrace {
                id: number as u64 + 1,
                started: Instant::now(),
                fields: serde_json::json!({"operation":"bulk","chunk":number,
                    "requestedBytes":chunk.min(frame - number * chunk)}),
            };
            for stage in ["submit", "submitted", "complete"] {
                enqueue_to(
                    &sender,
                    &dropped,
                    trace.record(stage, serde_json::json!({"receivedBytes":0})),
                );
            }
        }
        assert_eq!(dropped.load(Ordering::Relaxed), 0);
        for number in 0..117 {
            for stage in ["submit", "submitted", "complete"] {
                let UsbRecord::Line(line) = receiver.try_recv().unwrap() else {
                    panic!("expected record")
                };
                let record: serde_json::Value = serde_json::from_str(&line).unwrap();
                assert_eq!(record["level"], "info");
                assert_eq!(record["event"], format!("usb.{stage}"));
                assert_eq!(record["details"]["correlation"], number + 1);
                assert_eq!(record["details"]["chunk"], number);
                assert!(record["details"]["timestampUnixUs"].as_u64().unwrap() > 0);
                assert!(record["details"]["elapsedUs"].is_number());
                assert!(record["details"]["thread"].is_string());
                assert!(record["details"].get("payload").is_none());
            }
        }
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn saturated_log_queue_is_bounded_nonblocking_and_reports_exact_loss() {
        let (sender, receiver) = mpsc::sync_channel(2);
        let dropped = AtomicU64::new(0);
        for number in 0..5 {
            enqueue_to(&sender, &dropped, serde_json::json!({"number":number}));
        }
        assert_eq!(dropped.load(Ordering::Relaxed), 3);
        assert_eq!(
            dropped_record(dropped.load(Ordering::Relaxed))["details"]["droppedRecords"],
            3
        );
        assert!(matches!(receiver.try_recv(), Ok(UsbRecord::Line(_))));
        assert!(matches!(receiver.try_recv(), Ok(UsbRecord::Line(_))));
        assert!(receiver.try_recv().is_err());
        enqueue_to(&sender, &dropped, serde_json::json!({"number":5}));
        assert!(matches!(receiver.try_recv(), Ok(UsbRecord::Line(_))));
        assert_eq!(dropped.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn records_are_single_lines_and_logging_failure_is_nonfatal() {
        let mut output = Vec::new();
        let record = serde_json::json!({"version":1,"level":"warning","event":"transfer.retry",
            "message":"USB timeout\nretrying","pid":std::process::id()});
        write(&mut output, &record);
        let line = String::from_utf8(output).unwrap();
        assert_eq!(line.lines().count(), 1);
        let record: serde_json::Value =
            serde_json::from_str(line.trim().strip_prefix("REGAIN_DIAGNOSTIC ").unwrap()).unwrap();
        assert_eq!(record["event"], "transfer.retry");
        struct Closed;
        impl Write for Closed {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        write(&mut Closed, record);
    }
}
