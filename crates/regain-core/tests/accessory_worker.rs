//! Self-hosted process fixture: tests process retirement without Python, a shell,
//! USB access, or a separately installed worker. `--fixture` is child-only.
use regain_core::accessory::{AccessoryError, AccessoryWorker, MAX_RESPONSE_BYTES};
use serde_json::{Value, json};
use std::{
    io::{BufRead, Write},
    process::Stdio,
    time::Duration,
};

fn fixture() {
    for line in std::io::stdin().lock().lines() {
        let request: Value = serde_json::from_str(&line.unwrap()).unwrap();
        match request["command"].as_str().unwrap() {
            "hang" => std::thread::sleep(Duration::from_secs(30)),
            "malformed" => println!("not-json"),
            "oversized" => println!("{}", "x".repeat(MAX_RESPONSE_BYTES + 1)),
            "partial" => {
                print!("{{\"ok\":true");
                std::io::stdout().flush().unwrap();
                return;
            }
            "reject" => println!("{}", json!({"ok":false,"error":"invalid position"})),
            _ => println!("{}", json!({"ok":true,"result":request})),
        }
        std::io::stdout().flush().unwrap();
    }
}
fn spawn() -> AccessoryWorker {
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    command
        .arg("--fixture")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    AccessoryWorker::new(command.spawn().unwrap()).unwrap()
}
async fn retired(worker: &mut AccessoryWorker) {
    assert!(matches!(
        worker
            .request(json!({"command":"status"}))
            .await
            .unwrap_err()
            .downcast_ref::<AccessoryError>(),
        Some(AccessoryError::Disconnected)
    ));
    tokio::time::timeout(Duration::from_secs(3), worker.child.wait())
        .await
        .unwrap()
        .unwrap();
}
#[tokio::main]
async fn main() {
    if std::env::args().any(|arg| arg == "--fixture") {
        fixture();
        return;
    }
    let mut worker = spawn();
    assert_eq!(
        worker.request(json!({"command":"status"})).await.unwrap()["command"],
        "status"
    );
    assert!(matches!(
        worker
            .request(json!({"command":"reject"}))
            .await
            .unwrap_err()
            .downcast_ref::<AccessoryError>(),
        Some(AccessoryError::CommandFailed(_))
    ));
    assert!(matches!(
        worker
            .request(json!([]))
            .await
            .unwrap_err()
            .downcast_ref::<AccessoryError>(),
        Some(AccessoryError::InvalidRequest)
    ));
    assert_eq!(
        worker.request(json!({"command":"status"})).await.unwrap()["command"],
        "status"
    );
    worker.close().await;
    println!(
        "Accessory process: valid replies, rejected command, invalid request, graceful EOF passed"
    );
    for command in ["malformed", "oversized", "partial"] {
        let mut worker = spawn();
        assert!(matches!(
            worker
                .request(json!({"command":command}))
                .await
                .unwrap_err()
                .downcast_ref::<AccessoryError>(),
            Some(AccessoryError::Transport)
        ));
        retired(&mut worker).await;
        println!("Accessory process: {command} response retires child passed");
    }
    let mut worker = spawn();
    assert!(matches!(
        worker
            .request_with_timeout(json!({"command":"hang"}), Duration::from_millis(100))
            .await
            .unwrap_err()
            .downcast_ref::<AccessoryError>(),
        Some(AccessoryError::Timeout)
    ));
    retired(&mut worker).await;
    println!("Accessory process: deadline retires child passed");
    let mut worker = spawn();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(100),
            worker.request(json!({"command":"hang"}))
        )
        .await
        .is_err()
    );
    retired(&mut worker).await;
    println!("Accessory process: external cancellation retires child passed");
}
