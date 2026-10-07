use crate::{Diagnostic, Failure, invalid, process::ProcessGuard};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct Runtime {
    pub directory: PathBuf,
    pub sdk: PathBuf,
    pub simulate: bool,
    pub sdk_simulation: Option<Value>,
}
impl Runtime {
    pub async fn usb_command(&self, args: &[&str], seconds: u64) -> Result<String> {
        ensure!(
            !self.simulate,
            "Simulation must not launch USB recovery helpers"
        );
        let mut command = Command::new(
            self.directory
                .join(format!("regain-device{}", std::env::consts::EXE_SUFFIX)),
        );
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        let output = tokio::time::timeout(Duration::from_secs(seconds), command.output())
            .await
            .context("USB helper deadline exceeded")??;
        ensure!(
            output.status.success(),
            "USB helper failed ({}; Windows may require administrator approval): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    }
    pub async fn spawn(&self, direct: bool, log: Diagnostic) -> Result<Worker> {
        let path = self
            .directory
            .join(format!("regain-device{}", std::env::consts::EXE_SUFFIX));
        let mut command = Command::new(&path);
        command
            .args([
                "zwo",
                if direct {
                    "camera-direct"
                } else {
                    "camera-sdk"
                },
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .current_dir(&self.directory);
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        if direct {
            command.arg("--serve");
        }
        if self.simulate {
            command.arg("--simulate");
        } else if !direct {
            command.arg("--sdk").arg(&self.sdk);
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("Start {}", path.display()))?;
        let pid = child.id().context("Worker has no process ID")?;
        let guard = ProcessGuard::attach(pid)?;
        let stderr = child.stderr.take().context("Worker stderr unavailable")?;
        let diagnostic = tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(text) = line.strip_prefix("REGAIN_DIAGNOSTIC ")
                    && let Ok(record) = serde_json::from_str::<Value>(text)
                {
                    log(
                        record["level"].as_str().unwrap_or("info"),
                        record["event"].as_str().unwrap_or("worker"),
                        record["message"].as_str().unwrap_or(&line),
                    );
                } else {
                    log("info", "worker", &line);
                }
            }
        });
        let mut worker = Worker {
            input: child.stdin.take().context("Worker stdin unavailable")?,
            output: child.stdout.take().context("Worker stdout unavailable")?,
            child,
            _guard: guard,
            diagnostic,
            id: 0,
        };
        if self.simulate
            && !direct
            && let Some(settings) = &self.sdk_simulation
        {
            worker
                .call(
                    "simulation",
                    settings.clone(),
                    5.,
                    &CancellationToken::new(),
                )
                .await?;
        }
        Ok(worker)
    }
    /// SDK discovery is active hardware probing, even in a short-lived worker:
    /// ASIGetCameraProperty can internally open cameras owned by other processes.
    /// Cache results and schedule SDK discovery before imaging, not as live polling.
    /// Worker isolation bounds user-space calls, not side effects on shared USB.
    pub async fn list(
        &self,
        direct: bool,
        log: Diagnostic,
        token: &CancellationToken,
    ) -> Result<Value> {
        let mut worker = self.spawn(direct, log).await?;
        let result = worker
            .call("list", Value::Null, 20., token)
            .await
            .map(|r| r.0);
        worker.kill().await;
        result
    }
}
pub struct Worker {
    child: Child,
    input: ChildStdin,
    output: ChildStdout,
    _guard: ProcessGuard,
    diagnostic: tokio::task::JoinHandle<()>,
    id: u64,
}
impl Worker {
    /// Query (None) or configure shared software white balance while idle.
    /// Inspect the open response's `whiteBalance.supported` capability first.
    pub async fn white_balance(
        &mut self,
        settings: Option<crate::white_balance::Settings>,
        token: &CancellationToken,
    ) -> Result<Value> {
        if let Some(settings) = settings {
            settings.gains.validate()?;
        }
        Ok(self
            .call("white-balance", serde_json::to_value(settings)?, 15., token)
            .await?
            .0)
    }
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }
    pub async fn kill(&mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }
    pub async fn call(
        &mut self,
        method: &str,
        params: Value,
        seconds: f64,
        token: &CancellationToken,
    ) -> Result<(Value, Vec<u8>)> {
        self.call_bounded(method, params, seconds, token, 512 * 1024 * 1024)
            .await
    }
    /// Bound an image reply by the caller's admitted ROI before allocating pixels.
    /// A zero-length streaming poll remains valid; capture checks exact length.
    pub async fn call_image(
        &mut self,
        method: &str,
        params: Value,
        seconds: f64,
        token: &CancellationToken,
        maximum_bytes: usize,
    ) -> Result<(Value, Vec<u8>)> {
        ensure!(
            matches!(method, "download" | "stream-download" | "stream-poll")
                && (1..=512 * 1024 * 1024).contains(&maximum_bytes),
            Failure::Invalid("Invalid worker image admission".into())
        );
        self.call_bounded(method, params, seconds, token, maximum_bytes)
            .await
    }
    async fn call_bounded(
        &mut self,
        method: &str,
        params: Value,
        seconds: f64,
        token: &CancellationToken,
        maximum_bytes: usize,
    ) -> Result<(Value, Vec<u8>)> {
        self.call_until(
            method,
            params,
            tokio::time::Instant::now() + Duration::from_secs_f64(seconds),
            token,
            maximum_bytes,
        )
        .await
    }
    pub(crate) async fn cooling_call(
        &mut self,
        method: &str,
        params: Value,
        deadline: tokio::time::Instant,
        token: &CancellationToken,
    ) -> Result<(Value, Vec<u8>)> {
        self.call_until(method, params, deadline, token, 0).await
    }
    async fn call_until(
        &mut self,
        method: &str,
        params: Value,
        deadline: tokio::time::Instant,
        token: &CancellationToken,
        maximum_bytes: usize,
    ) -> Result<(Value, Vec<u8>)> {
        let cooling = method == "set" && matches!(params["control"].as_i64(), Some(16 | 17));
        let mut dispatched = false;
        let result = tokio::select! {
            biased;
            _=token.cancelled()=>Err(Failure::Cancelled.into()),
            result=tokio::time::timeout_at(deadline,self.exchange(method,params,maximum_bytes,&mut dispatched,deadline))=>result.unwrap_or_else(|_|{
                if cooling && !dispatched {Err(crate::cooling::CoolingError::Expired.into())}
                else {Err(anyhow::anyhow!("Worker {method} acknowledgement timed out"))}
            }),
        };
        let result = result.map_err(|error| {
            if cooling
                && dispatched
                && !matches!(
                    error.downcast_ref::<Failure>(),
                    Some(Failure::Worker { .. } | Failure::UncertainControl { .. })
                )
            {
                // The process may have consumed a partial/complete command. A
                // timeout, cancellation or malformed/lost reply cannot prove
                // this persistent hardware write was not applied.
                Failure::UncertainControl {
                    message: format!("{error:#}"),
                    code: None,
                }
                .into()
            } else {
                error
            }
        });
        if result.is_err()
            && !(!dispatched
                && result.as_ref().err().is_some_and(|error| {
                    matches!(
                        error.downcast_ref::<crate::cooling::CoolingError>(),
                        Some(crate::cooling::CoolingError::Expired)
                    )
                }))
            && !matches!(
                result
                    .as_ref()
                    .err()
                    .and_then(|e| e.downcast_ref::<Failure>()),
                Some(Failure::Worker { .. })
            )
        {
            self.kill().await;
        }
        result
    }
    async fn exchange(
        &mut self,
        method: &str,
        params: Value,
        maximum_bytes: usize,
        dispatched: &mut bool,
        deadline: tokio::time::Instant,
    ) -> Result<(Value, Vec<u8>)> {
        let id = self
            .id
            .checked_add(1)
            .ok_or_else(|| invalid("Worker command counter exhausted"))?;
        let bytes =
            serde_json::to_vec(&json!({"version":1,"id":id,"method":method,"params":params}))?;
        ensure!(
            bytes.len() <= 65536,
            Failure::Invalid("Command too large".into())
        );
        ensure!(
            tokio::time::Instant::now() < deadline,
            crate::cooling::CoolingError::Expired
        );
        self.id = id;
        *dispatched = true;
        self.input.write_u32_le(bytes.len() as u32).await?;
        self.input.write_all(&bytes).await?;
        self.input.flush().await?;
        read_reply(&mut self.output, method, self.id, maximum_bytes).await
    }
}

async fn read_reply<R: AsyncRead + Unpin>(
    output: &mut R,
    method: &str,
    id: u64,
    maximum_bytes: usize,
) -> Result<(Value, Vec<u8>)> {
    let length = output.read_u32_le().await? as usize;
    ensure!(
        (1..=65536).contains(&length),
        Failure::Invalid("Invalid worker response length".into())
    );
    let mut header = vec![0; length];
    output.read_exact(&mut header).await?;
    let reply: Value =
        serde_json::from_slice(&header).map_err(|_| invalid("Malformed worker JSON"))?;
    ensure!(
        reply["id"] == id && reply["version"] == 1,
        Failure::Invalid("Stale worker response".into())
    );
    let count = reply["binaryLength"]
        .as_u64()
        .ok_or_else(|| invalid("Missing frame length"))?;
    ensure!(
        count <= maximum_bytes as u64
            && count <= 512 * 1024 * 1024
            && (matches!(method, "download" | "stream-download" | "stream-poll") || count == 0),
        Failure::Invalid("Invalid worker image length".into())
    );
    if reply["ok"] == false {
        ensure!(
            count == 0,
            Failure::Invalid("Error response contains pixels".into())
        );
        let message = reply["error"].as_str().unwrap_or("Worker error").into();
        let code = reply["sdkCode"]
            .as_i64()
            .and_then(|v| i32::try_from(v).ok());
        return Err(if reply["controlUncertain"] == true {
            Failure::UncertainControl { message, code }
        } else {
            Failure::Worker { message, code }
        }
        .into());
    }
    ensure!(
        reply["ok"] == true,
        Failure::Invalid("Missing worker status".into())
    );
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(count as usize)
        .context("Worker image allocation failed")?;
    pixels.resize(count as usize, 0);
    output.read_exact(&mut pixels).await?;
    // Move the result out, avoiding a second decoded metadata tree.
    let mut reply = reply;
    Ok((reply["result"].take(), pixels))
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.diagnostic.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn simulated_runtime(settings: Value) -> Runtime {
        Runtime {
            directory: std::env::var_os("REGAIN_TEST_WORKERS")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug")
                }),
            sdk: "unused".into(),
            simulate: true,
            sdk_simulation: Some(settings),
        }
    }

    fn response(count: u64, ok: bool) -> Vec<u8> {
        let header = serde_json::to_vec(&json!({
            "version":1,"id":7,"ok":ok,"binaryLength":count,
            "result":{"width":3,"height":2,"readRecoveries":2},
            "error":"private SDK error","sdkCode":11,
        }))
        .unwrap();
        let mut bytes = (header.len() as u32).to_le_bytes().to_vec();
        bytes.extend(header);
        bytes
    }

    #[tokio::test]
    async fn cooling_transport_loss_retires_simulated_worker_without_retry() {
        let runtime = simulated_runtime(json!({"instant":true,"fault":"hang"}));
        for cancel_after_dispatch in [false, true] {
            let token = CancellationToken::new();
            let mut worker = runtime
                .spawn(false, std::sync::Arc::new(|_, _, _| {}))
                .await
                .unwrap();
            worker
                .call("open", json!({"name":"ZWO Simulated"}), 15., &token)
                .await
                .unwrap();
            worker.call("start",json!({"width":64,"height":64,"bin":1,"x":0,"y":0,"microseconds":1000,"dark":false}),15.,&token).await.unwrap();
            // Deliberately park the simulated owner in download. Its serial
            // command queue then cannot acknowledge the following cooler write.
            worker.id += 1;
            let header = serde_json::to_vec(
                &json!({"version":1,"id":worker.id,"method":"download","params":null}),
            )
            .unwrap();
            worker
                .input
                .write_u32_le(header.len() as u32)
                .await
                .unwrap();
            worker.input.write_all(&header).await.unwrap();
            worker.input.flush().await.unwrap();
            let mut call =
                Box::pin(worker.call("set", json!({"control":16,"value":-10}), 1., &token));
            if cancel_after_dispatch {
                // Poll through write admission before cancellation, rather than
                // using a scheduling-sensitive sleep to guess dispatch timing.
                std::future::poll_fn(|cx| {
                    assert!(std::future::Future::poll(call.as_mut(), cx).is_pending());
                    std::task::Poll::Ready(())
                })
                .await;
                token.cancel();
            }
            let error = call.await.unwrap_err();
            assert!(
                matches!(
                    error.downcast_ref::<Failure>(),
                    Some(Failure::UncertainControl { .. })
                ),
                "{error:#}"
            );
            assert!(!crate::retryable(&error));
            assert!(worker.child.try_wait().unwrap().is_some());
        }
        // Before-dispatch cancellation is still distinguishable from a write
        // whose acknowledgement was lost. No command ID is consumed.
        let mut worker = runtime
            .spawn(false, std::sync::Arc::new(|_, _, _| {}))
            .await
            .unwrap();
        let id = worker.id;
        let token = CancellationToken::new();
        token.cancel();
        let error = worker
            .call("set", json!({"control":17,"value":0}), 1., &token)
            .await
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<Failure>(),
            Some(Failure::Cancelled)
        ));
        assert_eq!(worker.id, id);
        assert!(worker.child.try_wait().unwrap().is_some());
    }

    #[tokio::test]
    async fn expired_cooler_write_preserves_worker_and_framing_without_dispatch() {
        let mut worker = simulated_runtime(json!({"instant":true}))
            .spawn(false, std::sync::Arc::new(|_, _, _| {}))
            .await
            .unwrap();
        let token = CancellationToken::new();
        let id = worker.id;
        let error = worker
            .cooling_call(
                "set",
                json!({"control":16,"value":-10}),
                tokio::time::Instant::now(),
                &token,
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<crate::cooling::CoolingError>(),
            Some(crate::cooling::CoolingError::Expired)
        ));
        assert_eq!(worker.id, id);
        assert!(worker.child.try_wait().unwrap().is_none());
        worker
            .call("open", json!({"name":"ZWO Simulated"}), 15., &token)
            .await
            .unwrap();
        worker
            .call("close", Value::Null, 15., &token)
            .await
            .unwrap();
        worker.kill().await;
    }

    #[tokio::test]
    async fn uncertain_control_reply_is_typed_and_never_retryable() {
        for code in [Some(11), None] {
            let header=serde_json::to_vec(&json!({"version":1,"id":7,"ok":false,
                "binaryLength":0,"error":"private cooling failure","sdkCode":code,"controlUncertain":true})).unwrap();
            let mut bytes = (header.len() as u32).to_le_bytes().to_vec();
            bytes.extend(header);
            let error = read_reply(&mut bytes.as_slice(), "set", 7, 0)
                .await
                .unwrap_err();
            assert!(
                matches!(error.downcast_ref::<Failure>(),Some(Failure::UncertainControl {code:actual,message}) if *actual==code && message=="private cooling failure")
            );
            assert!(!crate::retryable(&error));
        }
    }

    #[tokio::test]
    async fn oversized_admitted_reply_is_rejected_before_reading_any_body() {
        let (mut writer, mut reader) = tokio::io::duplex(1024);
        writer.write_all(&response(13, true)).await.unwrap();
        // Writer stays open and sends no body: waiting for pixels would time out.
        let error = tokio::time::timeout(
            Duration::from_secs(1),
            read_reply(&mut reader, "download", 7, 12),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(
            matches!(error.downcast_ref::<Failure>(), Some(Failure::Invalid(message)) if message == "Invalid worker image length")
        );
        assert!(!crate::retryable(&error));
    }

    #[tokio::test]
    async fn bounded_reply_preserves_pixels_metadata_and_empty_stream_poll() {
        let mut bytes = response(12, true);
        bytes.extend(0u8..12);
        let (metadata, pixels) = read_reply(&mut bytes.as_slice(), "download", 7, 12)
            .await
            .unwrap();
        assert_eq!(pixels, (0u8..12).collect::<Vec<_>>());
        assert_eq!(metadata["readRecoveries"], 2);
        let bytes = response(0, true);
        assert!(
            read_reply(&mut bytes.as_slice(), "stream-poll", 7, 12)
                .await
                .unwrap()
                .1
                .is_empty()
        );
    }

    #[tokio::test]
    async fn bounded_reply_keeps_error_codes_and_rejects_stale_or_non_image_pixels() {
        let bytes = response(0, false);
        let error = read_reply(&mut bytes.as_slice(), "download", 7, 12)
            .await
            .unwrap_err();
        assert!(
            matches!(error.downcast_ref::<Failure>(), Some(Failure::Worker {code:Some(11), message}) if message == "private SDK error")
        );
        for (count, ok, method, id) in [
            (1, false, "download", 7),
            (1, true, "status", 7),
            (0, true, "download", 8),
        ] {
            let bytes = response(count, ok);
            let error = read_reply(&mut bytes.as_slice(), method, id, 12)
                .await
                .unwrap_err();
            assert!(matches!(
                error.downcast_ref::<Failure>(),
                Some(Failure::Invalid(_))
            ));
        }
        let bytes = response(12, true);
        let error = read_reply(&mut bytes.as_slice(), "download", 7, 12)
            .await
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
    }
}
