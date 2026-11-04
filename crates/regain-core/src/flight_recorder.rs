//! Bounded metadata flight recorder. Only the diagnostic writer touches disk.
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs::{self, File},
    io::{BufWriter, Write},
    path::PathBuf,
    time::{Duration, Instant},
};

const LIMIT: usize = 8 * 1024 * 1024;
const SEGMENT: u64 = 4 * 1024 * 1024;

pub(super) struct Recorder {
    lines: VecDeque<String>,
    bytes: usize,
    limit: usize,
    sequence: u64,
    last_dump: Option<Instant>,
    disk: Option<DiskRing>,
}
impl Recorder {
    pub fn open() -> Self {
        let directory = std::env::var_os("REGAIN_CAMERA_DIAGNOSTICS_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("LOCALAPPDATA")
                    .map(|p| PathBuf::from(p).join("Regain/Diagnostics"))
            })
            .or_else(|| {
                std::env::var_os("XDG_STATE_HOME")
                    .map(|p| PathBuf::from(p).join("regain/diagnostics"))
            })
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|p| PathBuf::from(p).join(".local/state/regain/diagnostics"))
            })
            .unwrap_or_else(|| std::env::temp_dir().join("regain-diagnostics"));
        Self {
            lines: VecDeque::new(),
            bytes: 0,
            limit: LIMIT,
            sequence: 0,
            last_dump: None,
            disk: DiskRing::open(directory).ok(),
        }
    }
    pub fn path(&self) -> Option<String> {
        self.disk
            .as_ref()
            .map(|disk| disk.current.to_string_lossy().into_owned())
    }
    pub fn flush(&mut self) {
        if let Some(disk) = &mut self.disk {
            let _ = disk.writer.flush();
        }
    }
    pub fn clear(&mut self) {
        self.lines.clear();
        self.bytes = 0;
    }
    pub fn after_fault(&self, now: Instant) -> Option<String> {
        self.last_dump
            .filter(|prior| now.duration_since(*prior) < Duration::from_secs(5))
            .map(|_| format!("{}:{}", std::process::id(), self.sequence))
    }
    pub fn remember(&mut self, line: &str) {
        if line.len() > self.limit {
            return;
        }
        while self.bytes + line.len() > self.limit {
            if let Some(old) = self.lines.pop_front() {
                self.bytes -= old.len();
            } else {
                break;
            }
        }
        self.bytes += line.len();
        self.lines.push_back(line.to_owned());
        if let Some(disk) = &mut self.disk {
            let _ = disk.append(line);
        }
    }
    pub fn fault(&mut self, trigger: &Value, now: Instant) -> Vec<Value> {
        // One burst covers a failing request and its cleanup. Keep collecting
        // subsequent records, without replaying thousands for every cancelled URB.
        if self
            .last_dump
            .is_some_and(|prior| now.duration_since(prior) < Duration::from_secs(2))
        {
            return vec![];
        }
        self.last_dump = Some(now);
        self.sequence += 1;
        self.flush();
        let batch = format!("{}:{}", std::process::id(), self.sequence);
        let mut records = vec![
            json!({"version":1,"level":"warning","event":"diagnostics.batch.begin",
            "message":"Recent camera flight recorder history follows", "pid":std::process::id(),
            "details":{"batch":batch,"records":self.lines.len(),"bytes":self.bytes,"trigger":trigger["event"],"path":self.path()}}),
        ];
        for line in self.lines.drain(..) {
            if let Ok(mut record) = serde_json::from_str::<Value>(&line) {
                record["flightRecorderBatch"] = json!(batch);
                records.push(record);
            }
        }
        self.bytes = 0;
        records.push(json!({"version":1,"level":"info","event":"diagnostics.batch.end",
            "message":"End of camera flight recorder history","pid":std::process::id(),"details":{"batch":batch}}));
        records
    }
}

pub(super) fn fault(record: &Value) -> bool {
    matches!(record["level"].as_str(), Some("warning" | "error"))
        // A request can finish successfully after its deadline and CancelIoEx.
        // Raw Win32 success does not undo that timeout or establish whether a
        // control write took effect. Ordinary cancelled read-ahead has no
        // expired deadline and remains quiet.
        || record["details"]["deadlineExpired"] == true
        || record["details"]["waitError"].as_u64().is_some_and(|code| code != 0)
        || record["event"]
            .as_str()
            .is_some_and(|event| event.ends_with(".failed"))
        || record["details"]["win32Error"]
            .as_u64()
            // Cancelled SDK read-ahead buffers are normal after a completed
            // frame or explicit abort. An actual API/capture failure still
            // triggers its own batch; pending/timeout polls are also expected.
            .is_some_and(|code| !matches!(code, 0 | 258 | 995 | 996 | 997))
        || (record["event"] == "asi6200.register_snapshot"
            && matches!(
                record["details"]["stage"].as_str(),
                Some("read_failure" | "capture_failure")
            ))
        || (record["event"] == "sdk.api.complete"
            && record["details"]["result"]["returnValue"]
                .as_i64()
                .is_some_and(|code| code != 0)
            && record["details"]["api"] != "ASIGetControlCaps"
            && !(record["details"]["api"] == "ASIGetNumOfConnectedCameras"
                && record["details"]["result"]["returnValue"]
                    .as_i64()
                    .is_some_and(|count| (0..=128).contains(&count))))
}

struct DiskRing {
    current: PathBuf,
    previous: PathBuf,
    writer: BufWriter<File>,
    bytes: u64,
}
impl DiskRing {
    fn open(directory: PathBuf) -> std::io::Result<Self> {
        fs::create_dir_all(&directory)?;
        // Retain recent inactive workers; never prune a live worker's files.
        let mut files: Vec<_> = fs::read_dir(&directory)?
            .flatten()
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.starts_with("camera-")
                    && name.ends_with(".jsonl")
                    && name
                        .strip_prefix("camera-")
                        .and_then(|s| s.split('-').next())
                        .and_then(|s| s.parse::<u32>().ok())
                        .is_some_and(|pid| !process_alive(pid))
            })
            .collect();
        files.sort_by_key(|entry| entry.metadata().and_then(|m| m.modified()).ok());
        let excess = files.len().saturating_sub(14);
        for entry in files.into_iter().take(excess) {
            let _ = fs::remove_file(entry.path());
        }
        let current = directory.join(format!("camera-{}-current.jsonl", std::process::id()));
        let previous = directory.join(format!("camera-{}-previous.jsonl", std::process::id()));
        Ok(Self {
            writer: BufWriter::new(File::create(&current)?),
            current,
            previous,
            bytes: 0,
        })
    }
    fn append(&mut self, line: &str) -> std::io::Result<()> {
        if line.len() as u64 + 1 > SEGMENT {
            return Err(std::io::Error::other(
                "Diagnostic record exceeds disk segment limit",
            ));
        }
        if self.bytes + line.len() as u64 + 1 > SEGMENT {
            self.writer.flush()?;
            // Copy instead of renaming an open Windows file. Rotation runs only
            // on this writer thread; the USB owner never waits for disk I/O.
            fs::copy(&self.current, &self.previous)?;
            self.writer = BufWriter::new(File::create(&self.current)?);
            self.bytes = 0;
        }
        writeln!(self.writer, "{line}")?;
        self.bytes += line.len() as u64 + 1;
        Ok(())
    }
}

fn process_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, GetLastError, STILL_ACTIVE},
            System::Threading::{
                GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            },
        };
        // SAFETY: read-only process query; close the owned handle exactly once.
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if process.is_null() {
                return GetLastError() != ERROR_INVALID_PARAMETER;
            }
            let mut code = 0;
            let queried = GetExitCodeProcess(process, &mut code);
            CloseHandle(process);
            queried == 0 || code == STILL_ACTIVE as u32
        }
    }
    #[cfg(unix)]
    {
        // SAFETY: signal zero queries existence without sending a signal.
        unsafe {
            libc::kill(pid as libc::pid_t, 0) == 0
                || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_is_bounded_ordered_and_replayed_only_on_fault() {
        let mut recorder = Recorder {
            lines: VecDeque::new(),
            bytes: 0,
            limit: 600,
            sequence: 0,
            last_dump: None,
            disk: None,
        };
        for id in 0..20 {
            recorder.remember(&json!({"version":1,"level":"info","event":"usb.complete","message":"read","id":id}).to_string());
        }
        assert!(recorder.bytes <= 600);
        assert!(recorder.lines.len() < 20);
        let trigger =
            json!({"version":1,"level":"warning","event":"transfer.retry","message":"failed"});
        assert!(fault(&trigger));
        assert!(!fault(&json!({"level":"info","event":"capture.phase"})));
        assert!(!fault(
            &json!({"level":"info","event":"usb.complete","details":{"win32Error":995}})
        ));
        for raw_result in [0, 995] {
            assert!(fault(&json!({"level":"info","event":"usb.complete",
                "details":{"win32Error":raw_result,"deadlineExpired":true,"cancelRequested":true}})));
        }
        assert!(!fault(&json!({"level":"info","event":"usb.complete",
            "details":{"win32Error":995,"deadlineExpired":false,"cancelRequested":true}})));
        assert!(fault(&json!({"level":"info","event":"usb.complete",
            "details":{"win32Error":995,"deadlineExpired":false,"waitError":6,"cancelRequested":true}})));
        assert!(fault(
            &json!({"level":"info","event":"usb.complete","details":{"win32Error":31}})
        ));
        assert!(!fault(
            &json!({"level":"info","event":"sdk.api.complete","details":{"api":"ASIGetNumOfConnectedCameras","result":{"returnValue":2}}})
        ));
        assert!(fault(
            &json!({"level":"info","event":"sdk.api.complete","details":{"api":"ASIGetNumOfConnectedCameras","result":{"returnValue":-1}}})
        ));
        assert!(fault(
            &json!({"level":"info","event":"sdk.api.complete","details":{"api":"ASIGetDataAfterExp","result":{"returnValue":11}}})
        ));
        let now = Instant::now();
        let batch = recorder.fault(&trigger, now);
        assert_eq!(batch[0]["event"], "diagnostics.batch.begin");
        assert_eq!(batch.last().unwrap()["event"], "diagnostics.batch.end");
        assert_eq!(batch[batch.len() - 2]["id"], 19);
        assert!(batch[1]["flightRecorderBatch"].is_string());
        assert_eq!(recorder.bytes, 0);
        recorder.remember(&trigger.to_string());
        assert!(
            recorder
                .fault(&trigger, now + Duration::from_secs(1))
                .is_empty()
        );
        assert!(
            !recorder
                .fault(&trigger, now + Duration::from_secs(3))
                .is_empty()
        );
    }
    #[test]
    fn disk_rotates_without_unbounded_growth() {
        let directory = std::env::temp_dir().join(format!(
            "regain-flight-recorder-test-{}",
            std::process::id()
        ));
        let mut disk = DiskRing::open(directory.clone()).unwrap();
        let line = "x".repeat(1024);
        for _ in 0..10000 {
            disk.append(&line).unwrap();
        }
        disk.writer.flush().unwrap();
        assert!(fs::metadata(&disk.current).unwrap().len() <= SEGMENT);
        assert!(fs::metadata(&disk.previous).unwrap().len() <= SEGMENT);
        drop(disk);
        assert!(
            directory
                .canonicalize()
                .unwrap()
                .starts_with(std::env::temp_dir().canonicalize().unwrap())
        );
        fs::remove_dir_all(directory).unwrap();
    }
}
