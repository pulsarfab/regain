//! Log records use stderr only; they never change command replies or pixels.
use std::{fmt::Display, io::Write};

pub fn log(level: &str, event: &str, message: impl Display) {
    write(&mut std::io::stderr().lock(), level, event, message);
}

pub fn details(level: &str, event: &str, message: impl Display, details: serde_json::Value) {
    let _ = writeln!(
        std::io::stderr().lock(),
        "REGAIN_DIAGNOSTIC {}",
        serde_json::json!({"version":1,"level":level,"event":event,
        "message":message.to_string(),"pid":std::process::id(),"details":details})
    );
}

fn write(out: &mut impl Write, level: &str, event: &str, message: impl Display) {
    // Diagnostics are best effort, including a closed or failed stderr pipe.
    let _ = writeln!(
        out,
        "REGAIN_DIAGNOSTIC {}",
        serde_json::json!({
            "version":1,"level":level,"event":event,"message":message.to_string(),"pid":std::process::id()
        })
    );
}

pub fn read_failure(camera: &str, error: impl Display, used: usize, limit: u32, retained: bool) {
    let retry = used < limit as usize;
    let action = if retry {
        format!(
            "retry {}/{limit}: {}",
            used + 1,
            if retained {
                "rereading the same frame from byte zero"
            } else {
                "resynchronizing the guide stream with a subsequent frame"
            }
        )
    } else {
        "read retry limit reached".into()
    };
    log(
        "warning",
        if retry {
            "transfer.retry"
        } else {
            "transfer.exhausted"
        },
        format_args!("{camera}: {action}; {error}"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn records_are_single_lines_and_logging_failure_is_nonfatal() {
        let mut output = Vec::new();
        write(
            &mut output,
            "warning",
            "transfer.retry",
            "USB timeout\nretrying",
        );
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
        write(&mut Closed, "warning", "transfer.retry", "still capturing");
    }
}
