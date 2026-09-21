use anyhow::{Context, Result, ensure};
use regain_alpaca::{
    profile::Profiles,
    server::{Log, Server, discovery},
};
use regain_core::{CancellationToken, Runtime};
use std::{collections::HashMap, net::Ipv4Addr, path::PathBuf, sync::Arc};

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut options = HashMap::new();
    while let Some(arg) = args.next() {
        if arg == "--help" {
            println!(
                "PulsarFab regain ASCOM Alpaca (Rust)\n  --listen 127.0.0.1   IPv4 address (0.0.0.0 for LAN)\n  --port 11111\n  --profiles PATH     Saved equipment profiles\n  --workers DIRECTORY Rust workers and SDK\n  --sdk PATH          SDK library override\n  --simulate          Simulated equipment\n  --no-discovery      Disable UDP discovery\n  --stdio             Private pipe frontend\n  --backend sdk|direct Backend for private pipe frontend\nOpen http://127.0.0.1:11111/setup to configure equipment."
            );
            return Ok(());
        }
        ensure!(
            matches!(
                arg.as_str(),
                "--listen"
                    | "--port"
                    | "--profiles"
                    | "--workers"
                    | "--sdk"
                    | "--simulate"
                    | "--no-discovery"
                    | "--stdio"
                    | "--backend"
            ),
            "Unknown option: {arg}"
        );
        let value = if matches!(arg.as_str(), "--simulate" | "--no-discovery" | "--stdio") {
            String::new()
        } else {
            args.next()
                .with_context(|| format!("Missing value for {arg}"))?
        };
        options.insert(arg, value);
    }
    let executable = std::env::current_exe()?;
    let directory = options
        .get("--workers")
        .map(PathBuf::from)
        .unwrap_or(executable.parent().unwrap().to_path_buf())
        .canonicalize()?;
    let sdk = options.get("--sdk").map(PathBuf::from).unwrap_or_else(|| {
        directory.join(if cfg!(windows) {
            "ASICamera2.dll"
        } else if cfg!(target_os = "macos") {
            "libASICamera2.dylib"
        } else {
            "libASICamera2.so"
        })
    });
    let runtime = Runtime {
        directory,
        sdk: std::path::absolute(sdk)?,
        simulate: options.contains_key("--simulate"),
        sdk_simulation: None,
    };
    if options.contains_key("--stdio") {
        let backend = options
            .get("--backend")
            .map(String::as_str)
            .unwrap_or("sdk");
        ensure!(matches!(backend, "sdk" | "direct"), "Unknown backend");
        return regain_alpaca::stdio::run(
            runtime,
            backend == "direct",
            Log::new(None).diagnostic(None),
        )
        .await;
    }
    let address: Ipv4Addr = options
        .get("--listen")
        .map(String::as_str)
        .unwrap_or("127.0.0.1")
        .parse()?;
    let port: u16 = options
        .get("--port")
        .map(String::as_str)
        .unwrap_or("11111")
        .parse()?;
    ensure!(port > 0, "Port must be between 1 and 65535");
    let path = match options.get("--profiles") {
        Some(path) => PathBuf::from(path),
        None => {
            let base = if cfg!(windows) {
                std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
            } else {
                std::env::var_os("XDG_CONFIG_HOME")
                    .map(PathBuf::from)
                    .or_else(|| std::env::var_os("HOME").map(|v| PathBuf::from(v).join(".config")))
            };
            regain_alpaca::branding::migrate_profiles(&base.unwrap_or_else(|| PathBuf::from(".")))?
        }
    };
    let log = Log::new(Some(
        path.parent()
            .unwrap_or(std::path::Path::new("."))
            .join("logs"),
    ));
    let server = Server::new(Arc::new(Profiles::new(Some(path))?), runtime, log.clone());
    let stop = CancellationToken::new();
    let poll = tokio::spawn(server.clone().poll(stop.clone()));
    let discovery = if !options.contains_key("--no-discovery") {
        let stop = stop.clone();
        let log = log.clone();
        Some(tokio::spawn(async move {
            if let Err(e) = discovery(address, port, stop).await {
                (log.diagnostic(None))(
                    "warning",
                    "discovery.unavailable",
                    &format!("{e:#}; connect using the HTTP address"),
                );
            }
        }))
    } else {
        None
    };
    let listener = tokio::net::TcpListener::bind((address, port)).await?;
    println!("PulsarFab regain Alpaca listening on http://{address}:{port}/setup");
    let result = axum::serve(listener, server.router())
        .with_graceful_shutdown(shutdown_signal())
        .await;
    stop.cancel();
    let _ = poll.await;
    if let Some(task) = discovery {
        let _ = task.await;
    }
    server.shutdown().await;
    result?;
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
