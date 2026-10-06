//! Separate-process ownership/crash and framed IPC fixtures. No device I/O.
use regain_hub::{
    config::HubConfig,
    endpoint::Endpoint,
    factory::NoCredentials,
    ipc::{Limits, read_frame, serve_stream},
    native::NativeRuntime,
    runtime::HubRuntime,
    safety::MonotonicClock,
};
use serde_json::{Value, json};
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdout, Command},
};
use uuid::Uuid;

async fn fixture(mode: &str, path: &Path) {
    let endpoint = Endpoint::for_config(path).unwrap();
    let Some(owner) = endpoint.try_lock().unwrap() else {
        println!("busy");
        std::io::stdout().flush().unwrap();
        return;
    };
    if mode == "serve" {
        let mut listener = owner.bind().unwrap();
        let config: HubConfig = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let runtime = HubRuntime::build(
            config,
            &NativeRuntime {
                directory: path.parent().unwrap().into(),
                simulate: false,
                references: None,
            },
            &NoCredentials,
            Arc::new(MonotonicClock::default()),
        )
        .unwrap();
        println!("ready");
        std::io::stdout().flush().unwrap();
        let stream = listener.accept().await.unwrap();
        serve_stream(stream, runtime.clone(), Limits::default())
            .await
            .unwrap();
        runtime.shutdown().await.unwrap();
    } else {
        println!("locked");
        std::io::stdout().flush().unwrap();
        if mode == "hold" {
            let _ = tokio::io::stdin().read_u8().await;
        } else {
            assert_eq!(mode, "probe");
        }
        drop(owner);
    }
}
struct FixtureProcess {
    child: Child,
    output: BufReader<ChildStdout>,
}
fn spawn(mode: &str, path: &Path) -> FixtureProcess {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .arg("--fixture")
        .arg(mode)
        .arg(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut child = command.spawn().unwrap();
    let output = BufReader::new(child.stdout.take().unwrap());
    FixtureProcess { child, output }
}
impl FixtureProcess {
    async fn line(&mut self) -> String {
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(10), self.output.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        line.trim().to_owned()
    }
    async fn success(&mut self) {
        assert!(
            tokio::time::timeout(Duration::from_secs(10), self.child.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
    }
}

async fn tests() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.json");
    let config = json!({"schemaVersion":1,"revision":Uuid::new_v4(),"instanceId":Uuid::new_v4(),"sources":[],"outputs":[]});
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    let endpoint = Endpoint::for_config(&path).unwrap();
    let mut owner = spawn("hold", &path);
    assert_eq!(owner.line().await, "locked");
    let mut competitor = spawn("probe", &path);
    assert_eq!(competitor.line().await, "busy");
    competitor.success().await;
    let replacement = dir.path().join("replacement.json");
    std::fs::write(&replacement, serde_json::to_vec(&config).unwrap()).unwrap();
    std::fs::rename(&replacement, &path).unwrap();
    let mut competitor = spawn("probe", &path);
    assert_eq!(competitor.line().await, "busy");
    competitor.success().await;
    owner.child.kill().await.unwrap();
    assert!(endpoint.try_lock().unwrap().is_some());
    let mut after_crash = spawn("probe", &path);
    assert_eq!(after_crash.line().await, "locked");
    after_crash.success().await;

    let mut server = spawn("serve", &path);
    assert_eq!(server.line().await, "ready");
    assert!(endpoint.try_lock().unwrap().is_none());
    let mut client = endpoint.connect(Duration::from_secs(5)).await.unwrap();
    for (id, op) in [(1, "hello"), (2, "listDevices")] {
        let bytes = serde_json::to_vec(&json!({"version":1,"id":id,"command":{"op":op}})).unwrap();
        client
            .write_all(&(bytes.len() as u32).to_le_bytes())
            .await
            .unwrap();
        client.write_all(&bytes).await.unwrap();
        let bytes = tokio::time::timeout(
            Duration::from_secs(5),
            read_frame(&mut client, Duration::from_secs(2)),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        let reply: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(reply["id"], id);
        if id == 1 {
            assert_eq!(reply["result"]["instanceId"], config["instanceId"]);
        } else {
            assert_eq!(reply["result"], json!([]));
        }
    }
    drop(client);
    server.success().await;
    assert!(endpoint.try_lock().unwrap().is_some());
    println!(
        "Endpoint process fixtures passed: competing owner, atomic replacement, crash release, IPC handshake and EOF cleanup."
    );
}
fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    if args.get(1).is_some_and(|arg| arg == "--fixture") {
        let mode = args[2].to_str().unwrap();
        let path = PathBuf::from(&args[3]);
        runtime.block_on(fixture(mode, &path));
    } else {
        runtime.block_on(tests());
    }
}
