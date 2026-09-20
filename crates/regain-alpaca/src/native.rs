//! Private local-camera pipe. Shares camera semantics and recovery, never starts HTTP.
use crate::{
    device::{Device, Error, Params},
    profile::{Profile, Profiles},
    server::Log,
};
use anyhow::{Result, ensure};
use regain_core::{CancellationToken, Runtime};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

async fn request_length(
    input: &mut (impl AsyncRead + Unpin),
    first: bool,
) -> Result<Option<usize>> {
    let mut header = [0; 4];
    if input.read(&mut header[..1]).await? == 0 {
        return Ok(None);
    }
    input.read_exact(&mut header[1..]).await?;
    // .NET Framework's Process.StandardInput may write a UTF-8 BOM before
    // the caller accesses BaseStream. Accept it only at the start of the pipe.
    // It cannot be confused with a valid length (maximum 64 KiB).
    if first && header[..3] == [0xef, 0xbb, 0xbf] {
        header[0] = header[3];
        input.read_exact(&mut header[1..]).await?;
    }
    let length = u32::from_le_bytes(header);
    ensure!((1..=65536).contains(&length), "Invalid request length");
    Ok(Some(length as usize))
}

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
        let mut first = true;
        loop {
            let Some(length) = request_length(&mut input, first).await? else { break };
            first = false;
            let mut bytes = vec![0; length]; input.read_exact(&mut bytes).await?;
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

#[cfg(test)]
mod tests {
    use super::request_length;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn pipe_accepts_optional_initial_bom_without_shifting_frames() {
        for prefix in [b"".as_slice(), b"\xef\xbb\xbf".as_slice()] {
            let mut bytes = prefix.to_vec();
            bytes.extend_from_slice(&2_u32.to_le_bytes());
            bytes.extend_from_slice(b"{}");
            bytes.extend_from_slice(&3_u32.to_le_bytes());
            bytes.extend_from_slice(b"123");
            let mut input = bytes.as_slice();
            assert_eq!(request_length(&mut input, true).await.unwrap(), Some(2));
            let mut first = [0; 2];
            input.read_exact(&mut first).await.unwrap();
            assert_eq!(&first, b"{}");
            assert_eq!(request_length(&mut input, false).await.unwrap(), Some(3));
            let mut second = [0; 3];
            input.read_exact(&mut second).await.unwrap();
            assert_eq!(&second, b"123");
            assert_eq!(request_length(&mut input, false).await.unwrap(), None);
        }
    }

    #[tokio::test]
    async fn pipe_rejects_truncated_headers_invalid_lengths_and_late_bom() {
        for bytes in [
            b"\x01".as_slice(),
            b"\xef\xbb\xbf".as_slice(),
            b"\xef\xbb\xbf\x01".as_slice(),
            &[0, 0, 0, 0],
            &[1, 0, 1, 0],
        ] {
            assert!(request_length(&mut &bytes[..], true).await.is_err());
        }
        assert!(
            request_length(&mut &b"\xef\xbb\xbf\x01\x00\x00\x00"[..], false)
                .await
                .is_err()
        );
    }
}
