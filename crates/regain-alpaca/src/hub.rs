//! Private hub mode of the shared executable. No HTTP or discovery listener.
use anyhow::{Context, Result, bail};
use regain_core::CancellationToken;
use regain_hub::{
    config::ConfigStore, endpoint::Endpoint, factory::NoCredentials, host, ipc::Limits,
    native::NativeRuntime, runtime::HubRuntime, safety::MonotonicClock, service::HubService,
};
use std::{future::Future, path::Path, sync::Arc, time::Duration};

pub async fn run(
    config: &Path,
    native: NativeRuntime,
    shutdown: impl Future<Output = ()>,
) -> Result<()> {
    let endpoint = Endpoint::for_config(config).context("Resolve private hub endpoint")?;
    // Lock before loading/preparing any source. A losing launcher only probes
    // the existing host and exits; it never builds a second set of source actors.
    let owner = endpoint.try_lock().context("Acquire hub ownership")?;
    let store = ConfigStore::load(endpoint.config_path())
        .map_err(|_| anyhow::anyhow!("Cannot load a valid hub configuration"))?;
    let config = store.snapshot();
    let Some(owner) = owner else {
        let hello = host::probe(&endpoint, config.instance_id, Duration::from_secs(10))
            .await
            .context("Existing hub did not pass its readiness check")?;
        println!("Regain hub already running: {}", hello.host_instance);
        return Ok(());
    };
    let listener = owner.bind().context("Bind private hub endpoint")?;
    // Credential references fail closed until the OS-protected provider lands.
    let state = HubService::persistent(
        store,
        Arc::new(move |config| {
            HubRuntime::build(
                config,
                &native,
                &NoCredentials,
                Arc::new(MonotonicClock::default()),
            )
        }),
    )
    .map_err(|errors| {
        anyhow::anyhow!(
            "Hub configuration cannot be hosted: {}",
            errors
                .iter()
                .map(|error| format!("{}: {}", error.path, error.message))
                .collect::<Vec<_>>()
                .join("; ")
        )
    })?;
    let stop = CancellationToken::new();
    let service = host::serve_service(listener, state.clone(), Limits::default(), stop.clone());
    tokio::pin!(service);
    println!("Regain hub ready: {} (local IPC only)", state.host_id());
    let result = tokio::select! {
        result = &mut service => result,
        _ = shutdown => { stop.cancel(); service.await }
    };
    if let Err(error) = result {
        bail!("{error}; {} source cleanup errors", error.cleanup.len());
    }
    Ok(())
}
