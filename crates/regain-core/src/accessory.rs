//! Shared client for the newline-JSON accessory workers in `regain-device`.
//! Native camera workers keep their separate transport/recovery supervisor. COM
//! imports can append a finite binary body under the same process request guard.
use crate::process::ProcessGuard;
use anyhow::{Context, Result};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::{fmt, future::Future, pin::Pin, time::Duration};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout},
};

pub const MAX_REQUEST_BYTES: usize = 4096;
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// Distinguish a framed worker error from transport loss. A worker error may
/// still follow a dispatched USB command; neither permits automatic replay.
#[derive(Debug)]
pub enum AccessoryError {
    InvalidRequest,
    Disconnected,
    Transport,
    Timeout,
    CommandFailed(String),
}
impl fmt::Display for AccessoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest => f.write_str("Invalid or oversized accessory request"),
            Self::Disconnected => f.write_str("Accessory worker is disconnected"),
            Self::Transport => {
                f.write_str("Accessory worker response failed; command was not retried")
            }
            Self::Timeout => f.write_str("Accessory worker timed out; command was not retried"),
            Self::CommandFailed(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for AccessoryError {}

pub struct AccessoryWorker {
    pub child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
    _guard: ProcessGuard,
}
impl AccessoryWorker {
    /// Adopt a child with piped stdin/stdout. Windows ownership also attaches a
    /// kill-on-close job so a dead parent cannot strand an exclusive device owner.
    pub fn new(child: Child) -> Result<Self> {
        Self::adopt(child, false)
    }
    /// COM imports own the private Regain worker, not vendor helpers it launches.
    pub fn new_independent(child: Child) -> Result<Self> {
        Self::adopt(child, true)
    }
    fn adopt(mut child: Child, independent: bool) -> Result<Self> {
        let pipes = (|| {
            let input = child.stdin.take().context("Accessory stdin unavailable")?;
            let output = child
                .stdout
                .take()
                .context("Accessory stdout unavailable")?;
            let pid = child.id().context("Accessory process exited")?;
            let guard = if independent {
                ProcessGuard::attach_independent(pid)?
            } else {
                ProcessGuard::attach(pid)?
            };
            Ok::<_, anyhow::Error>((input, output, guard))
        })();
        match pipes {
            Ok((input, output, guard)) => Ok(Self {
                child,
                input: Some(input),
                output: BufReader::new(output),
                _guard: guard,
            }),
            Err(error) => {
                let _ = child.start_kill();
                Err(error)
            }
        }
    }
    pub async fn request(&mut self, request: Value) -> Result<Value> {
        self.request_with_timeout(request, Duration::from_secs(10))
            .await
    }
    pub async fn request_with_timeout(
        &mut self,
        request: Value,
        deadline: Duration,
    ) -> Result<Value> {
        self.request_typed_with_timeout(request, deadline).await
    }
    /// Deserialize the inner reply directly from its frame. Typed protocols
    /// must reject duplicate fields before conversion to a generic JSON map.
    pub async fn request_typed_with_timeout<T: DeserializeOwned>(
        &mut self,
        request: Value,
        deadline: Duration,
    ) -> Result<T> {
        let bytes = encode_request(request)?;
        if self.input.is_none() {
            return Err(AccessoryError::Disconnected.into());
        }
        // This guard lives inside the awaited operation. Timeout OR cancellation
        // closes stdin and retires the process immediately, preventing a delayed
        // response from being consumed as the next command's acknowledgement.
        let operation = async {
            let mut guard = PendingRequest {
                child: &mut self.child,
                input: &mut self.input,
                armed: true,
            };
            guard
                .input
                .as_mut()
                .unwrap()
                .write_all(&bytes)
                .await
                .map_err(|_| AccessoryError::Transport)?;
            let bytes = read_response(&mut self.output).await?;
            let result = decode_typed_response(&bytes);
            if result.is_ok() || matches!(result, Err(AccessoryError::CommandFailed(_))) {
                guard.armed = false;
            }
            result
        };
        tokio::time::timeout(deadline, operation)
            .await
            .unwrap_or(Err(AccessoryError::Timeout))
            .map_err(Into::into)
    }
    /// One acknowledged frame followed by a caller-defined finite binary body.
    /// The same process/cancellation guard spans both; a rejected header, partial
    /// body, failed allocation, timeout or dropped future retires the stream.
    /// The callback must consume and validate the entire body before succeeding.
    pub async fn request_typed_with_stream<T, R, F>(
        &mut self,
        request: Value,
        deadline: Duration,
        body: F,
    ) -> Result<R>
    where
        T: DeserializeOwned,
        F: for<'a> FnOnce(
            T,
            &'a mut BufReader<ChildStdout>,
        ) -> Pin<Box<dyn Future<Output = Result<R>> + Send + 'a>>,
    {
        let bytes = encode_request(request)?;
        if self.input.is_none() {
            return Err(AccessoryError::Disconnected.into());
        }
        let operation = async {
            let mut guard = PendingRequest {
                child: &mut self.child,
                input: &mut self.input,
                armed: true,
            };
            guard
                .input
                .as_mut()
                .unwrap()
                .write_all(&bytes)
                .await
                .map_err(|_| AccessoryError::Transport)?;
            let bytes = read_response(&mut self.output).await?;
            let header = match decode_typed_response(&bytes) {
                Ok(header) => header,
                Err(error) => {
                    if matches!(error, AccessoryError::CommandFailed(_)) {
                        guard.armed = false;
                    }
                    return Err(error.into());
                }
            };
            let result = body(header, &mut self.output).await;
            if result.is_ok() {
                guard.armed = false;
            }
            result
        };
        tokio::time::timeout(deadline, operation)
            .await
            .unwrap_or_else(|_| Err(AccessoryError::Timeout.into()))
    }
    /// Retire a stream whose outstanding request may have been cancelled.
    pub fn reset(&mut self) {
        self.input.take();
        let _ = self.child.start_kill();
    }
    pub async fn close(mut self) {
        self.input.take();
        if !matches!(
            tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await,
            Ok(Ok(_))
        ) {
            let _ = self.child.kill().await;
        }
    }
}
impl Drop for AccessoryWorker {
    fn drop(&mut self) {
        self.reset();
    }
}
struct PendingRequest<'a> {
    child: &'a mut Child,
    input: &'a mut Option<ChildStdin>,
    armed: bool,
}
impl Drop for PendingRequest<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.input.take();
            let _ = self.child.start_kill();
        }
    }
}
fn encode_request(request: Value) -> Result<Vec<u8>, AccessoryError> {
    if !request.is_object() {
        return Err(AccessoryError::InvalidRequest);
    }
    let mut bytes = serde_json::to_vec(&request).map_err(|_| AccessoryError::InvalidRequest)?;
    if bytes.len() >= MAX_REQUEST_BYTES {
        return Err(AccessoryError::InvalidRequest);
    }
    bytes.push(b'\n');
    Ok(bytes)
}
async fn read_response(input: &mut (impl AsyncBufRead + Unpin)) -> Result<Vec<u8>, AccessoryError> {
    let mut line = Vec::new();
    loop {
        let buffer = input
            .fill_buf()
            .await
            .map_err(|_| AccessoryError::Transport)?;
        if buffer.is_empty() {
            return Err(AccessoryError::Transport);
        }
        let end = buffer.iter().position(|byte| *byte == b'\n').map(|n| n + 1);
        let length = end.unwrap_or(buffer.len());
        if length > MAX_RESPONSE_BYTES - line.len() {
            return Err(AccessoryError::Transport);
        }
        line.extend_from_slice(&buffer[..length]);
        input.consume(length);
        if end.is_some() {
            return Ok(line);
        }
    }
}
#[derive(Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
struct Reply<T> {
    ok: bool,
    #[serde(default, deserialize_with = "present_result")]
    result: Option<T>,
    error: Option<String>,
}
fn present_result<'de, T: Deserialize<'de>, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}
#[cfg(test)]
fn decode_response(bytes: &[u8]) -> Result<Value, AccessoryError> {
    decode_typed_response(bytes)
}
fn decode_typed_response<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, AccessoryError> {
    let reply: Reply<T> = serde_json::from_slice(bytes).map_err(|_| AccessoryError::Transport)?;
    if reply.ok {
        reply.result.ok_or(AccessoryError::Transport)
    } else {
        Err(AccessoryError::CommandFailed(
            reply
                .error
                .unwrap_or_else(|| "Accessory command failed".into()),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn typed_reply_rejects_duplicate_inner_fields_and_keeps_null_values() {
        #[derive(Deserialize)]
        struct Typed {
            id: u64,
        }
        assert_eq!(
            decode_typed_response::<Typed>(br#"{"ok":true,"result":{"id":1}}"#)
                .unwrap()
                .id,
            1
        );
        assert!(matches!(
            decode_typed_response::<Typed>(br#"{"ok":true,"result":{"id":1,"id":2}}"#),
            Err(AccessoryError::Transport)
        ));
        assert_eq!(
            decode_response(br#"{"ok":true,"result":null}"#).unwrap(),
            Value::Null
        );
        assert!(decode_response(br#"{"ok":true}"#).is_err());
    }

    #[tokio::test]
    async fn framing_bounds_partial_eof_and_multiple_responses() {
        let mut input = &b"{\"ok\":true,\"result\":12}\n{\"ok\":true,\"result\":13}\n"[..];
        assert_eq!(
            decode_response(&read_response(&mut input).await.unwrap()).unwrap(),
            12
        );
        assert_eq!(
            decode_response(&read_response(&mut input).await.unwrap()).unwrap(),
            13
        );
        assert!(read_response(&mut input).await.is_err());
        assert!(read_response(&mut &b"{\"ok\":true}"[..]).await.is_err());
        let large = vec![b'x'; MAX_RESPONSE_BYTES + 1];
        assert!(read_response(&mut &large[..]).await.is_err());
    }
    #[test]
    fn request_bounds_and_worker_errors() {
        assert!(encode_request(json!([])).is_err());
        assert!(encode_request(json!({"command":"x".repeat(MAX_REQUEST_BYTES)})).is_err());
        assert_eq!(
            encode_request(json!({"command":"status"})).unwrap().last(),
            Some(&b'\n')
        );
        assert!(
            matches!(decode_response(br#"{"ok":false,"error":"busy"}"#), Err(AccessoryError::CommandFailed(message)) if message == "busy")
        );
        for bytes in [
            br#"{"ok":"true"}"#.as_slice(),
            br#"{"ok":true,"ok":false}"#,
            b"bad",
        ] {
            assert!(matches!(
                decode_response(bytes),
                Err(AccessoryError::Transport)
            ));
        }
    }
}
