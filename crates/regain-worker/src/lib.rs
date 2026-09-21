//! Shared, bounded newline-JSON IPC for accessory workers.
//! Camera workers retain their separate length-prefixed JSON and binary frame protocol.
use anyhow::Result;
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, Write},
    sync::mpsc,
    time::Duration,
};

pub const MAX_REQUEST: usize = 4096;
/// EOF, a partial final request, and oversized input terminate the session.
/// This bounds memory before allocation, including when a caller never sends a newline.
pub fn read_request(input: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let buf = input.fill_buf()?;
        if buf.is_empty() {
            return Ok(None);
        }
        let end = buf.iter().position(|b| *b == b'\n').map(|n| n + 1);
        let n = end.unwrap_or(buf.len());
        if line.len() + n > MAX_REQUEST {
            return Err(io::ErrorKind::InvalidData.into());
        }
        line.extend_from_slice(&buf[..n]);
        input.consume(n);
        if end.is_some() {
            return Ok(Some(line));
        }
    }
}
pub fn stdin_requests() -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::sync_channel(16);
    std::thread::spawn(move || {
        let mut input = io::stdin().lock();
        while let Ok(Some(line)) = read_request(&mut input) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}
pub fn parse(line: &[u8]) -> Result<Value> {
    Ok(serde_json::from_str(
        std::str::from_utf8(line)?.trim_start_matches('\u{feff}'),
    )?)
}
pub fn reply(result: Result<Value>) -> Value {
    match result {
        Ok(value) => json!({"ok":true,"result":value}),
        Err(error) => json!({"ok":false,"error":format!("{error:#}")}),
    }
}
/// Write one response without allowing a closed parent pipe to panic the worker.
pub fn write_reply(result: Result<Value>) -> io::Result<()> {
    let response = reply(result);
    let mut output = io::stdout().lock();
    writeln!(output, "{response}").and_then(|_| output.flush())
}
/// Dispatch requests and poll idle devices. The caller retains device-specific shutdown policy.
pub fn serve<S>(
    state: &mut S,
    interval: Duration,
    mut request: impl FnMut(&mut S, Value) -> Result<Value>,
    mut poll: impl FnMut(&mut S),
) {
    let rx = stdin_requests();
    loop {
        match rx.recv_timeout(interval) {
            Ok(line) => {
                if write_reply(parse(&line).and_then(|v| request(state, v))).is_err() {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => poll(state),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn framing_bounds_and_bom() {
        let mut input = &b"\xef\xbb\xbf{\"command\":\"status\"}\n{}\n"[..];
        assert_eq!(
            parse(&read_request(&mut input).unwrap().unwrap()).unwrap()["command"],
            "status"
        );
        assert_eq!(
            parse(&read_request(&mut input).unwrap().unwrap()).unwrap(),
            json!({})
        );
        assert!(read_request(&mut input).unwrap().is_none());
        assert!(read_request(&mut &b"{}"[..]).unwrap().is_none());
        assert!(read_request(&mut &vec![b'x'; MAX_REQUEST + 1][..]).is_err());
        assert!(parse(b"\xff\n").is_err());
        assert_eq!(reply(parse(b"bad"))["ok"], false);
        let mut exact = vec![b' '; MAX_REQUEST];
        exact[MAX_REQUEST - 1] = b'\n';
        assert_eq!(
            read_request(&mut &exact[..]).unwrap().unwrap().len(),
            MAX_REQUEST
        );
    }
}
