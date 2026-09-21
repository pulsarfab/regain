use crate::{Diagnostic, Failure, invalid, process::ProcessGuard};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
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
        let result = tokio::select! {
            biased;
            _=token.cancelled()=>Err(Failure::Cancelled.into()),
            result=tokio::time::timeout(Duration::from_secs_f64(seconds),self.exchange(method,params))=>result.unwrap_or_else(|_|Err(anyhow::anyhow!("Worker {method} timed out after {seconds} seconds"))),
        };
        if result.is_err()
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
    async fn exchange(&mut self, method: &str, params: Value) -> Result<(Value, Vec<u8>)> {
        self.id += 1;
        let bytes =
            serde_json::to_vec(&json!({"version":1,"id":self.id,"method":method,"params":params}))?;
        ensure!(
            bytes.len() <= 65536,
            Failure::Invalid("Command too large".into())
        );
        self.input.write_u32_le(bytes.len() as u32).await?;
        self.input.write_all(&bytes).await?;
        self.input.flush().await?;
        let length = self.output.read_u32_le().await? as usize;
        ensure!(
            (1..=65536).contains(&length),
            Failure::Invalid("Invalid worker response length".into())
        );
        let mut header = vec![0; length];
        self.output.read_exact(&mut header).await?;
        let reply: Value =
            serde_json::from_slice(&header).map_err(|_| invalid("Malformed worker JSON"))?;
        ensure!(
            reply["id"] == self.id && reply["version"] == 1,
            Failure::Invalid("Stale worker response".into())
        );
        let count = reply["binaryLength"]
            .as_u64()
            .ok_or_else(|| invalid("Missing frame length"))?;
        ensure!(
            count <= 512 * 1024 * 1024 && (method == "download" || count == 0),
            Failure::Invalid("Invalid worker image length".into())
        );
        if reply["ok"] == false {
            ensure!(
                count == 0,
                Failure::Invalid("Error response contains pixels".into())
            );
            return Err(Failure::Worker {
                message: reply["error"].as_str().unwrap_or("Worker error").into(),
                code: reply["sdkCode"].as_i64().map(|v| v as i32),
            }
            .into());
        }
        ensure!(
            reply["ok"] == true,
            Failure::Invalid("Missing worker status".into())
        );
        let mut pixels = vec![0; count as usize];
        self.output.read_exact(&mut pixels).await?;
        Ok((reply["result"].clone(), pixels))
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.diagnostic.abort();
    }
}
