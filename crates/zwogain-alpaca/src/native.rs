//! Private local-camera pipe. Shares camera semantics and recovery, never starts HTTP.
use crate::{
    device::{Device, Error, Params},
    profile::{Profile, Profiles},
    server::Log,
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use zwogain_core::{CancellationToken, Runtime};

pub async fn run(runtime: Runtime, profiles: Arc<Profiles>, log: Arc<Log>) -> Result<()> {
    let device = Device::new(
        0,
        profiles.clone(),
        runtime.clone(),
        log.diagnostic(Some(0)),
    );
    let stop = CancellationToken::new();
    let poll_device = device.clone();
    let poll_stop = stop.clone();
    let poll = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = poll_stop.cancelled() => break,
                _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => poll_device.refresh().await,
            }
        }
    });
    let result = async {
        let mut input = tokio::io::stdin();
        let mut output = tokio::io::stdout();
        loop {
            let length = match input.read_u32_le().await {
                Ok(v) => v,
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e.into()),
            };
            ensure!((1..=65536).contains(&length), "Invalid request length");
            let mut bytes = vec![0; length as usize]; input.read_exact(&mut bytes).await?;
            let request: Value = serde_json::from_slice(&bytes)?;
            ensure!(request["version"] == 1, "Unsupported pipe protocol");
            let mut pixels: Arc<[u8]> = Arc::from([]);
            let response: Result<Value> = async {
                let member = request["member"].as_str().unwrap_or("");
                let p = &request["params"];
                match request["method"].as_str().unwrap_or("") {
                    "profile" => {
                        // Another Chooser instance may have saved since this client was created.
                        if !device.in_use() { profiles.reload()?; }
                        Ok(serde_json::to_value(profiles.get(0)?)?)
                    }
                    "configure" => { device.configure(serde_json::from_value::<Profile>(p.clone())?)?; Ok(Value::Null) }
                    "discover" => {
                        ensure!(!device.in_use(), Error(0x40B, "Disconnect before scanning USB".into()));
                        runtime.list(p["direct"] == true, log.diagnostic(None), &CancellationToken::new()).await
                    }
                    "get" if matches!(member, "imagearray" | "imagearrayvariant") => {
                        let frame = device.image(1)?; pixels = frame.pixels.clone();
                        Ok(json!({"width":frame.exposure.width,"height":frame.exposure.height}))
                    }
                    "get" => device.get(member, 1),
                    "put" => {
                        let mut values = BTreeMap::new();
                        if let Some(map) = p.as_object() { for (k,v) in map { values.insert(k.to_lowercase(), v.as_str().map(str::to_owned).unwrap_or_else(|| v.to_string())); }}
                        let params = Params(values);
                        if matches!(member, "connected" | "connect" | "disconnect") {
                            let on = if member == "connected" { params.boolean("Connected")? } else { member == "connect" };
                            if on && !device.in_use() {
                                profiles.reload()?;
                                if profiles.get(0)?.camera.is_none() {
                                    let mut profile = profiles.get(0)?;
                                    let found = runtime.list(profile.direct, log.diagnostic(None), &CancellationToken::new()).await?;
                                    let choices = found.as_array().ok_or_else(|| anyhow::anyhow!("Invalid camera list"))?;
                                    ensure!(choices.len() == 1, Error(0x40B, "Choose a camera in ASCOM setup".into()));
                                    profile.camera = Some(choices[0].clone()); device.configure(profile)?;
                                }
                            }
                            device.connected(1, on, member != "connected").await?; Ok(Value::Null)
                        } else if member == "abortexposure" { device.abort(1).await?; Ok(Value::Null) }
                        else { device.put(member, &params, 1) }
                    }
                    _ => anyhow::bail!("Unknown pipe operation"),
                }
            }.await;
            let header = match response {
                Ok(value) => json!({"version":1,"id":request["id"],"value":value,"binaryLength":pixels.len(),"errorNumber":0}),
                Err(e) => json!({"version":1,"id":request["id"],"errorNumber":crate::server::error_code(&e),"errorMessage":format!("{e:#}"),"binaryLength":0}),
            };
            let bytes = serde_json::to_vec(&header)?;
            output.write_u32_le(bytes.len() as u32).await?; output.write_all(&bytes).await?;
            output.write_all(&pixels).await?; output.flush().await?;
        }
        Ok(())
    }.await;
    stop.cancel();
    let _ = poll.await;
    device.shutdown().await;
    result
}
