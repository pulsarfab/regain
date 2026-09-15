use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Mutex as AsyncMutex,
};
use zwogain_core::{
    CancellationToken, Diagnostic, Exposure, Frame, RecoveryOptions, Runtime, Selection, Session,
    SharedStatus, invalid,
};

#[derive(Default)]
struct Capture {
    busy: bool,
    frame: Option<Arc<Frame>>,
    error: Option<String>,
    cancel: CancellationToken,
}
struct Supervisor {
    engine: Arc<AsyncMutex<Option<Session>>>,
    status: Option<SharedStatus>,
    capture: Arc<Mutex<Capture>>,
    task: Option<tokio::task::JoinHandle<()>>,
    runtime: Runtime,
    direct: bool,
    log: Diagnostic,
}
impl Supervisor {
    async fn abort(&mut self) {
        self.capture.lock().unwrap().cancel.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
        let mut c = self.capture.lock().unwrap();
        c.busy = false;
        c.frame = None;
        c.error = None;
    }
    async fn close(&mut self) {
        self.abort().await;
        if let Some(mut session) = self.engine.lock().await.take() {
            session.close().await;
        }
        self.status = None;
    }
    async fn command(&mut self, method: &str, p: Value) -> Result<(Value, Arc<[u8]>)> {
        let mut pixels: Arc<[u8]> = Arc::from([]);
        let token = CancellationToken::new();
        let value = match method {
            "list" => {
                self.runtime
                    .list(self.direct, self.log.clone(), &token)
                    .await?
            }
            "diagnostics" => serde_json::to_value(
                self.status
                    .as_ref()
                    .ok_or_else(|| invalid("Camera not open"))?
                    .lock()
                    .unwrap()
                    .clone(),
            )?,
            "open" => {
                ensure!(self.status.is_none(), "Camera already open");
                let selection = Selection {
                    name: p["name"].as_str().unwrap_or("").into(),
                    serial: p["serial"].as_str().map(str::to_owned),
                    direct: self.direct,
                    sdk_fallback: p["allowSdkFallback"] == true,
                    recovery: if p["recovery"].is_null() {
                        RecoveryOptions::default()
                    } else {
                        serde_json::from_value(p["recovery"].clone())?
                    },
                };
                let mut session = Session::new(selection, self.runtime.clone(), self.log.clone())?;
                if let Err(e) = session.connect(&token).await {
                    session.close().await;
                    return Err(e);
                }
                if p["recoveryState"]["settle"] == true {
                    session.seed_recovery(
                        p["recoveryState"]["temperature"].as_f64(),
                        p["recoveryState"]["power"].as_i64(),
                    );
                }
                let state = session.snapshot();
                self.status = Some(session.status.clone());
                *self.engine.lock().await = Some(session);
                json!({"info":state.info,"serial":state.serial,"sdkVersion":state.sdk_version,"backend":state.backend,"sdkFallback":state.sdk_fallback,"supervised":true,"controls":state.controls.values().collect::<Vec<_>>()})
            }
            "get" => {
                let status = self
                    .status
                    .as_ref()
                    .ok_or_else(|| invalid("Camera not open"))?
                    .lock()
                    .unwrap();
                json!(
                    status
                        .values
                        .get(
                            &(p["control"]
                                .as_i64()
                                .ok_or_else(|| invalid("Missing control"))?
                                as i32)
                        )
                        .ok_or_else(|| invalid("Control unavailable"))?
                )
            }
            "set" => {
                Session::queue_control(
                    self.status
                        .as_ref()
                        .ok_or_else(|| invalid("Camera not open"))?,
                    p["control"]
                        .as_i64()
                        .ok_or_else(|| invalid("Missing control"))? as i32,
                    p["value"]
                        .as_i64()
                        .ok_or_else(|| invalid("Missing control value"))?,
                )?;
                if let Ok(mut engine) = self.engine.try_lock()
                    && let Some(session) = engine.as_mut()
                {
                    session.refresh(&token).await?;
                }
                Value::Null
            }
            "start" => {
                let exposure: Exposure = serde_json::from_value(p)?;
                {
                    let status = self
                        .status
                        .as_ref()
                        .ok_or_else(|| invalid("Camera not open"))?
                        .lock()
                        .unwrap();
                    zwogain_core::validate_exposure(&status.info, &status.controls, &exposure)?;
                }
                let cancel = {
                    let mut c = self.capture.lock().unwrap();
                    ensure!(!c.busy, "Exposure already active");
                    *c = Capture {
                        busy: true,
                        ..Capture::default()
                    };
                    c.cancel.clone()
                };
                let engine = self.engine.clone();
                let capture = self.capture.clone();
                self.task = Some(tokio::spawn(async move {
                    let mut session = engine.lock().await;
                    let result = if let Some(session) = session.as_mut() {
                        session.capture(exposure, &cancel).await
                    } else {
                        Err(invalid("Disconnected"))
                    };
                    let mut c = capture.lock().unwrap();
                    if !cancel.is_cancelled() {
                        match result {
                            Ok(frame) => c.frame = Some(Arc::new(frame)),
                            Err(e) => c.error = Some(format!("{e:#}")),
                        }
                    }
                    c.busy = false;
                }));
                Value::Null
            }
            "status" => {
                let c = self.capture.lock().unwrap();
                let state = self
                    .status
                    .as_ref()
                    .ok_or_else(|| invalid("Camera not open"))?
                    .lock()
                    .unwrap();
                json!({"state":if c.busy{1}else if c.error.is_some(){3}else if c.frame.is_some(){2}else{0},"phase":state.phase,"error":c.error,"backend":state.backend,"sdkFallback":state.sdk_fallback,"snapshot":if c.busy {None} else {Some(&*state)}})
            }
            "download" => {
                let c = self.capture.lock().unwrap();
                ensure!(!c.busy, "Exposure still active");
                if let Some(error) = &c.error {
                    anyhow::bail!("{error}")
                }
                let frame = c
                    .frame
                    .as_ref()
                    .ok_or_else(|| invalid("No completed image"))?;
                pixels = frame.pixels.clone();
                frame.metadata.clone()
            }
            "abort" | "stop" => {
                self.abort().await;
                Value::Null
            }
            "close" => {
                self.close().await;
                Value::Null
            }
            _ => return Err(invalid(format!("Unknown supervisor command: {method}"))),
        };
        Ok((value, pixels))
    }
}
pub async fn run(runtime: Runtime, direct: bool, log: Diagnostic) -> Result<()> {
    let mut supervisor = Supervisor {
        engine: Arc::new(AsyncMutex::new(None)),
        status: None,
        capture: Arc::new(Mutex::new(Capture::default())),
        task: None,
        runtime,
        direct,
        log,
    };
    let engine = supervisor.engine.clone();
    let stop = CancellationToken::new();
    let poll_stop = stop.clone();
    let poll = tokio::spawn(async move {
        loop {
            tokio::select! {_=poll_stop.cancelled()=>break,_=tokio::time::sleep(std::time::Duration::from_secs(2))=>{if let Ok(mut owner)=engine.try_lock()&&let Some(session)=owner.as_mut(){let _=session.refresh(&poll_stop).await;}}}
        }
    });
    let result=async {
        let mut input=tokio::io::stdin();let mut output=tokio::io::stdout();
        loop {
            let length=match input.read_u32_le().await{Ok(v)=>v,Err(e)if e.kind()==std::io::ErrorKind::UnexpectedEof=>break,Err(e)=>return Err(e.into())};
            ensure!(length>0&&length<=65536,"Invalid request length");let mut bytes=vec![0;length as usize];input.read_exact(&mut bytes).await?;let request:Value=serde_json::from_slice(&bytes)?;ensure!(request["version"]==1,"Unsupported protocol");
            let response=supervisor.command(request["method"].as_str().unwrap_or(""),request["params"].clone()).await;
            let (header,pixels)=match response{Ok((value,pixels))=>(json!({"version":1,"id":request["id"],"ok":true,"result":value,"binaryLength":pixels.len()}),pixels),Err(e)=>{(supervisor.log)("warning","command.failed",&format!("{}: {e:#}",request["method"]));(json!({"version":1,"id":request["id"],"ok":false,"error":format!("{e:#}"),"sdkCode":8,"sdkOperation":"supervisor","binaryLength":0}),Arc::from([]))}};
            let bytes=serde_json::to_vec(&header)?;output.write_u32_le(bytes.len() as u32).await?;output.write_all(&bytes).await?;output.write_all(&pixels).await?;output.flush().await?;
        }
        Ok(())
    }.await;
    stop.cancel();
    let _ = poll.await;
    supervisor.close().await;
    result
}
