//! Keep exact ABI call assertions independent of the worker's background logger.
use std::fmt::Arguments;
use std::io::Write;

pub fn trace(call: Arguments<'_>) {
    let path = std::env::var_os("REGAIN_FIXTURE_SDK_TRACE").expect("SDK fixture trace path");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open SDK fixture trace");
    writeln!(file, "CALL {call}").expect("write SDK fixture trace");
}
