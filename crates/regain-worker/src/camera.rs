//! Bounded camera framing for a single hardware owner. Images follow JSON replies.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    sync::mpsc,
    time::Duration,
};

pub fn serve<S>(
    state: &mut S,
    mut command: impl FnMut(&mut S, &str, Value) -> Result<(Value, Vec<u8>)>,
    mut tick: impl FnMut(&mut S) -> Result<()>,
    error_details: impl Fn(&anyhow::Error) -> Value,
) -> Result<()> {
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut input = std::io::stdin().lock();
        loop {
            let request = (|| -> Result<Value> {
                let mut size = [0; 4];
                input.read_exact(&mut size)?;
                let length = u32::from_le_bytes(size) as usize;
                ensure!((1..=65536).contains(&length), "Invalid camera request size");
                let mut bytes = vec![0; length];
                input.read_exact(&mut bytes)?;
                let request: Value = serde_json::from_slice(&bytes)?;
                ensure!(request["version"] == 1, "Unsupported camera protocol");
                Ok(request)
            })();
            match request {
                Ok(v) => {
                    if tx.send(v).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    let mut output = std::io::stdout().lock();
    let mut tick_error = None;
    loop {
        let request = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(v) => v,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if tick_error.is_none()
                    && let Err(e) = tick(state)
                {
                    tick_error = Some((format!("{e:#}"), error_details(&e)));
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let method = request["method"]
            .as_str()
            .context("Missing camera method")?;
        let result = if let Some((message, _)) = &tick_error
            && !matches!(method, "close" | "stop" | "diagnostics-dump")
        {
            Err(anyhow::anyhow!(
                "Camera telemetry/control owner failed: {}",
                message
            ))
        } else {
            command(state, method, request["params"].clone())
        };
        let (reply, pixels) = match result {
            Ok((v, p)) => (
                json!({"version":1,"id":request["id"],"ok":true,"result":v,"binaryLength":p.len()}),
                p,
            ),
            Err(e) => {
                let mut reply = json!({"version":1,"id":request["id"],"ok":false,"error":format!("{e:#}"),"binaryLength":0});
                let details = tick_error
                    .as_ref()
                    .map(|(_, v)| v.clone())
                    .unwrap_or_else(|| error_details(&e));
                if let Some(fields) = details.as_object() {
                    for key in ["sdkCode", "controlUncertain"] {
                        if let Some(value) = fields.get(key) {
                            reply[key] = value.clone();
                        }
                    }
                }
                (reply, Vec::new())
            }
        };
        let data = serde_json::to_vec(&reply)?;
        ensure!(data.len() <= 65536, "Oversized camera response");
        if output
            .write_all(&(data.len() as u32).to_le_bytes())
            .and_then(|_| output.write_all(&data))
            .and_then(|_| output.write_all(&pixels))
            .and_then(|_| output.flush())
            .is_err()
        {
            break;
        }
        if method == "close" {
            tick_error = None;
        }
    }
    command(state, "close", Value::Null)?;
    Ok(())
}
